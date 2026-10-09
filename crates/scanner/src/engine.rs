//! The receive loop: Airspy samples in, speaker audio, recordings and
//! [`Event`]s out. Blocking; front ends run it on a thread of its own and
//! steer it through [`Controls`].

use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::Receiver;

use airspy::Airspy;
use chrono::Local;
use radiocore::{Channelizer, Complex32, NfmChannel, NfmConfig, P25Channel, P25Frame};

use crate::plan::{BIN_HZ, Entry, Kind, Listener, Plan};
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
/// Seconds of reception to throw away while the tuner settles: after the
/// device starts, and after each change of band.
const START_SETTLE_SECS: f64 = 0.3;
const RETUNE_SETTLE_SECS: f64 = 0.1;
/// Scanning one channel at a time: how long to look at a channel before
/// deciding nothing is there, and how far off the channel to tune so that it
/// isn't at the centre of the passband, where the tuner's own leakage lands.
const CHANNEL_CHECK_SECS: f32 = 0.05;
const CHANNEL_OFFSET_HZ: f64 = 300_000.0;

pub enum Source {
    /// Receive from the first Airspy found.
    Airspy,
    /// Read 16-bit I/Q from stdin.
    Stdin,
}

/// How the channels of a plan are covered.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ScanMode {
    /// Receive everything at once; refuse a plan too spread out for that.
    OneBand,
    /// Receive a whole band at once, and take turns between bands when the
    /// plan needs more than one.
    HopBands,
    /// Listen to one frequency at a time, moving on when it is quiet: slow
    /// to notice traffic, but light on the computer and the USB link.
    Channels,
}

pub struct Config {
    /// Airspy linearity gain, 0-21.
    pub gain: u8,
    /// Seconds to stay on a channel after it goes quiet.
    pub hold_secs: f32,
    /// Directory to save every transmission to, as WAV files.
    pub record: Option<PathBuf>,
    /// Directory to save the transmissions of channels marked for recording
    /// to, whether or not everything else is being saved.
    pub record_marked: Option<PathBuf>,
    /// Airspy sample rate; `None` for the fastest the device offers.
    /// Scanning one channel at a time always uses the slowest.
    pub rate: Option<u32>,
    pub source: Source,
    /// Play audio through the speakers.
    pub audio: bool,
    pub scan: ScanMode,
    /// Seconds to listen to a quiet band before moving to the next; when
    /// scanning one channel at a time, how long to wait on a signal for the
    /// channel to open.
    pub dwell_secs: f32,
    /// Seconds after which a band that stays busy is left anyway, between
    /// transmissions, so the other bands get a turn.
    pub max_stay_secs: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gain: 17,
            hold_secs: 1.5,
            record: None,
            record_marked: None,
            rate: None,
            source: Source::Airspy,
            audio: true,
            scan: ScanMode::HopBands,
            dwell_secs: 1.5,
            max_stay_secs: 20.0,
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
    priority: Vec<AtomicBool>,
    recorded: Vec<AtomicBool>,
    /// A recording to play (or `None` to stop playing one), until the engine
    /// picks the request up.
    replay_request: Mutex<Option<Option<PathBuf>>>,
    replaying: AtomicBool,
}

impl Controls {
    /// Controls for running `plan`, with its channels' saved skips and
    /// priorities applied.
    pub fn new(plan: &Plan, volume: f32, squelch_db: f32) -> Self {
        Self {
            stop: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            volume: AtomicU32::new(volume.to_bits()),
            squelch_db: AtomicU32::new(squelch_db.to_bits()),
            pinned: AtomicUsize::new(usize::MAX),
            skipped: plan.entries.iter().map(|e| AtomicBool::new(e.skip)).collect(),
            priority: plan.entries.iter().map(|e| AtomicBool::new(e.priority)).collect(),
            recorded: plan.entries.iter().map(|e| AtomicBool::new(e.record)).collect(),
            replay_request: Mutex::new(None),
            replaying: AtomicBool::new(false),
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

    /// Priority channels interrupt whatever else is playing.
    pub fn priority(&self, channel: usize) -> bool {
        self.priority[channel].load(Relaxed)
    }

    pub fn set_priority(&self, channel: usize, priority: bool) {
        self.priority[channel].store(priority, Relaxed);
    }

    /// Channels marked for recording have their calls saved to
    /// [`Config::record_marked`].
    pub fn recorded(&self, channel: usize) -> bool {
        self.recorded[channel].load(Relaxed)
    }

    pub fn set_recorded(&self, channel: usize, recorded: bool) {
        self.recorded[channel].store(recorded, Relaxed);
    }

    /// Play a recording made by this engine in place of live audio.
    pub fn replay(&self, recording: PathBuf) {
        *self.replay_request.lock().unwrap() = Some(Some(recording));
    }

    pub fn stop_replay(&self) {
        *self.replay_request.lock().unwrap() = Some(None);
    }

    /// Whether a recording is playing.
    pub fn replaying(&self) -> bool {
        self.replaying.load(Relaxed)
    }
}

pub enum Event<'a> {
    /// A transmission started; `playing` if it is the one on the speaker.
    /// For P25 calls, `talkgroup` and the calling radio's `unit` ID are
    /// given when the call carried them.
    Opened {
        channel: usize,
        snr_db: f32,
        playing: bool,
        talkgroup: Option<u16>,
        unit: Option<u32>,
    },
    /// A transmission ended after `secs` seconds; `recording` is its file
    /// if recording is on. `unit` is as in `Opened`, for calls that only
    /// named their radio partway through.
    Closed {
        channel: usize,
        secs: f32,
        unit: Option<u32>,
        recording: Option<PathBuf>,
    },
    /// The channel on the speaker changed.
    Playing(Option<usize>),
    /// Carrier level over the noise floor for every channel, in dB.
    Levels(&'a [f32]),
    /// Tuned to band `index` of `count`, covering `lo_hz` to `hi_hz`.
    Band {
        index: usize,
        count: usize,
        lo_hz: f64,
        hi_hz: f64,
    },
}

/// Sample rate used when the device can't be asked (reading from stdin).
const DEFAULT_RATE: u32 = 10_000_000;

/// The sample rates to choose from: the ones the attached Airspy offers.
/// The Airspy must not be in use when asking it.
pub fn sample_rates(cfg: &Config) -> Result<Vec<u32>, String> {
    match cfg.source {
        Source::Stdin => Ok(vec![cfg.rate.unwrap_or(DEFAULT_RATE)]),
        Source::Airspy => {
            let rates = Airspy::open()
                .and_then(|device| device.sample_rates())
                .map_err(|e| e.to_string())?;
            if rates.is_empty() {
                return Err("the Airspy offers no sample rates".into());
            }
            Ok(rates)
        }
    }
}

/// The sample rate `cfg` will run at: the slowest on offer when scanning
/// one channel at a time, otherwise the one it names or the fastest.
pub fn sample_rate(cfg: &Config) -> Result<u32, String> {
    let rates = sample_rates(cfg)?;
    match (cfg.scan, cfg.rate) {
        (ScanMode::Channels, _) => Ok(rates.into_iter().min().unwrap_or(DEFAULT_RATE)),
        (_, Some(rate)) if rates.contains(&rate) => Ok(rate),
        (_, Some(rate)) => {
            let offered: Vec<String> = rates.iter().map(|r| r.to_string()).collect();
            Err(format!(
                "this Airspy has no {rate} samples/s mode; it offers {}",
                offered.join(", ")
            ))
        }
        (_, None) => Ok(rates.into_iter().max().unwrap_or(DEFAULT_RATE)),
    }
}

/// Widest spread of frequencies, in Hz, one tuning can cover at `rate`.
pub fn usable_span_hz(rate: u32) -> f64 {
    rate as f64 * USABLE_BANDWIDTH
}

/// Where the samples come from.
enum Input {
    Stdin(BufReader<io::StdinLock<'static>>, Vec<u8>),
    Airspy {
        device: Airspy,
        blocks: Receiver<airspy::Block>,
        /// Samples received but not yet handed on.
        queue: VecDeque<i16>,
        rate: u32,
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
                let mut input = Input::Airspy {
                    device,
                    blocks,
                    queue: VecDeque::new(),
                    rate,
                };
                input.tune(center_hz, START_SETTLE_SECS)?;
                Ok(input)
            })()
            .map_err(|e: airspy::Error| e.to_string()),
        }
    }

    /// Change frequency, dropping what was received before and while the
    /// tuner settles.
    fn tune(&mut self, center_hz: f64, settle_secs: f64) -> Result<(), airspy::Error> {
        if let Input::Airspy {
            device,
            blocks,
            queue,
            rate,
        } = self
        {
            device.set_frequency(center_hz.round() as u32)?;
            queue.clear();
            while blocks.try_recv().is_ok() {}
            let mut settling = (*rate as f64 * settle_secs) as usize;
            while settling > 0 {
                let Ok(block) = blocks.recv() else { break };
                settling = settling.saturating_sub(block.iq.len() / 2);
            }
        }
        Ok(())
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
    /// Analog FM, feeding plan entry `row`.
    Nfm {
        channel: Box<NfmChannel>,
        row: usize,
    },
    P25(Box<P25Rx>),
}

/// A P25 frequency, and the call on it if there is one.
#[derive(Default)]
struct P25Rx {
    /// The trunked system this frequency belongs to, if it is one.
    system: i64,
    /// For a conventional channel: its plan entry, and the network access
    /// code calls must carry to count.
    fixed: Option<(usize, Option<u16>)>,
    channel: P25Channel,
    frames: Vec<P25Frame>,
    /// Plan entry this call is playing on.
    row: Option<usize>,
    talkgroup: Option<u16>,
    unit: Option<u32>,
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
            fixed: self.fixed,
            channel: std::mem::take(&mut self.channel),
            ..Self::default()
        };
    }
}

/// One tuning of the Airspy and the receivers that run while it is tuned
/// there.
struct Band {
    lo_hz: f64,
    hi_hz: f64,
    center_hz: f64,
    channelizer: Channelizer,
    rxs: Vec<Rx>,
    /// Upper bound on the analog channels' noise floors.
    floor_cap: f32,
}

/// Receive until the input ends or [`Controls::stop`] is called.
pub fn run(plan: &Plan, cfg: &Config, controls: &Controls, mut on_event: impl FnMut(Event)) -> Result<(), String> {
    if plan.frequencies().next().is_none() {
        return Err("nothing to scan: no channels selected".into());
    }
    let rate = sample_rate(cfg)?;
    if rate as f64 % (2.0 * BIN_HZ) != 0.0 {
        return Err("sample rate must be a multiple of 1000".into());
    }
    let by_channel = cfg.scan == ScanMode::Channels;
    // One channel at a time is the same as hopping between bands, with
    // every frequency a band of its own.
    let mut layout = plan.bands(if by_channel { 0.0 } else { usable_span_hz(rate) });
    if by_channel {
        layout.iter_mut().for_each(|band| band.center_hz += CHANNEL_OFFSET_HZ);
    }
    if layout.len() > 1 && cfg.scan == ScanMode::OneBand {
        let (lo, hi, _) = plan.span();
        return Err(format!(
            "these channels spread over {:.1} MHz and the Airspy covers {:.1} MHz at a time at this sample rate; \
             change the scan mode to hop between the {} bands",
            (hi - lo) / 1e6,
            usable_span_hz(rate) / 1e6,
            layout.len()
        ));
    }
    if layout.len() > 1 && matches!(cfg.source, Source::Stdin) {
        return Err("scanning more than one band needs an Airspy to retune; stdin carries one".into());
    }

    let rows = plan.entries.len();
    let block_len = CHANNEL_BINS / 2;
    let mut squelch_db = controls.squelch_db();
    // Which band each plan entry is heard on; a talkgroup's is the first
    // with one of its system's frequencies.
    let mut row_band: Vec<Option<usize>> = vec![None; rows];
    let mut bands: Vec<Band> = Vec::new();
    for (b, tuning) in layout.iter().enumerate() {
        let (mut offsets, mut rxs) = (Vec::new(), Vec::new());
        for &(freq_hz, listener) in &tuning.listeners {
            offsets.push(freq_hz - tuning.center_hz);
            rxs.push(match listener {
                Listener::Row(row) => {
                    row_band[row] = Some(b);
                    match plan.entries[row].kind {
                        Kind::Analog { tone_hz, narrow, .. } => Rx::Nfm {
                            channel: Box::new(NfmChannel::new(NfmConfig {
                                sample_rate: CHANNEL_RATE,
                                block_len,
                                max_deviation: if narrow { 2500.0 } else { 5000.0 },
                                tone_hz,
                                squelch: 10f32.powf(squelch_db / 10.0),
                            })),
                            row,
                        },
                        Kind::Digital { nac, .. } => Rx::P25(Box::new(P25Rx {
                            fixed: Some((row, nac)),
                            ..P25Rx::default()
                        })),
                        Kind::Talkgroup { .. } | Kind::OtherTalkgroups { .. } => unreachable!("not tied to a frequency"),
                    }
                }
                Listener::Trunked { system } => {
                    for (row, e) in plan.entries.iter().enumerate() {
                        let on_system = matches!(e.kind, Kind::Talkgroup { system: s, .. } | Kind::OtherTalkgroups { system: s } if s == system);
                        if on_system && row_band[row].is_none() {
                            row_band[row] = Some(b);
                        }
                    }
                    Rx::P25(Box::new(P25Rx {
                        system,
                        ..P25Rx::default()
                    }))
                }
            });
        }
        let channelizer = Channelizer::new(rate as f64, BIN_HZ, CHANNEL_BINS, IF_CUTOFF_HZ, &offsets);
        assert_eq!(block_len, channelizer.out_len());
        bands.push(Band {
            lo_hz: tuning.lo_hz,
            hi_hz: tuning.hi_hz,
            center_hz: tuning.center_hz,
            channelizer,
            rxs,
            floor_cap: f32::INFINITY,
        });
    }
    // The entry for a trunked call: its talkgroup's, or its system's catch-all.
    let talkgroup_row = |system: i64, talkgroup: Option<u16>| {
        let listed =
            |e: &Entry| matches!(e.kind, Kind::Talkgroup { system: s, id } if s == system && Some(id) == talkgroup);
        let other = |e: &Entry| matches!(e.kind, Kind::OtherTalkgroups { system: s } if s == system);
        plan.entries
            .iter()
            .position(listed)
            .or_else(|| plan.entries.iter().position(other))
    };

    for dir in cfg.record.iter().chain(&cfg.record_marked) {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut speaker = if cfg.audio {
        Some(audio::Output::open(AUDIO_RATE)?)
    } else {
        None
    };
    let mut input = Input::open(bands[0].center_hz, rate, cfg)?;

    let hop = bands[0].channelizer.hop();
    let audio_len = block_len / 3;
    let block_secs = block_len as f32 / CHANNEL_RATE;
    let hold_blocks = (cfg.hold_secs / block_secs) as u32;
    let dwell_blocks = (cfg.dwell_secs / block_secs) as u32;
    let max_stay_blocks = (cfg.max_stay_secs / block_secs) as u32;
    let check_blocks = (CHANNEL_CHECK_SECS / block_secs) as u32;
    let mut iq = vec![Complex32::ZERO; hop];
    let mut audio = vec![Vec::with_capacity(audio_len); rows];
    let mut open = vec![false; rows];
    let mut open_blocks = vec![0u32; rows];
    // Which receiver of the tuned band feeds each open entry.
    let mut row_rx: Vec<Option<usize>> = vec![None; rows];
    // The radio heard on each open P25 entry, once it has said who it is.
    let mut row_unit: Vec<Option<u32>> = vec![None; rows];
    let mut recorders: Vec<Option<(wav::Writer, PathBuf)>> = (0..rows).map(|_| None).collect();
    let mut pcm = Vec::with_capacity(audio_len);
    let silence = vec![0i16; audio_len];
    let mut floors = Vec::with_capacity(rows);
    let mut levels = vec![0.0; rows];
    let mut current: Option<usize> = None;
    let mut playing: Option<usize> = None;
    let mut replay: Option<(Vec<i16>, usize)> = None;
    let mut idle_blocks = 0u32;
    let mut band_idle_blocks = 0u32;
    let mut band_blocks = 0u32;
    let mut tuned = 0;
    let mut blocks = 0u64;
    let mut failure = None;

    let announce = |tuned: usize, bands: &[Band], on_event: &mut dyn FnMut(Event)| {
        on_event(Event::Band {
            index: tuned,
            count: bands.len(),
            lo_hz: bands[tuned].lo_hz,
            hi_hz: bands[tuned].hi_hz,
        });
    };
    announce(tuned, &bands, &mut on_event);

    while !controls.stop.load(Relaxed) && input.read(&mut iq) {
        let band = &mut bands[tuned];
        let floor_cap = band.floor_cap;
        let rxs = &mut band.rxs;
        band.channelizer.process(&iq, |i, samples| match &mut rxs[i] {
            Rx::Nfm { channel, row } => {
                audio[*row].clear();
                open[*row] = channel.process(samples, floor_cap, &mut audio[*row]) && !controls.skipped(*row);
                row_rx[*row] = Some(i);
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
                    P25Frame::Voice {
                        nac,
                        talkgroup,
                        source,
                        pcm,
                    } => {
                        let row = match rx.fixed {
                            Some((row, wanted)) => wanted.is_none_or(|w| w == nac).then_some(row),
                            // The talkgroup can arrive a frame after the voice does.
                            None => talkgroup_row(rx.system, talkgroup),
                        };
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
                        if let Some(row) = rx.row {
                            rx.unit = source.or(rx.unit);
                            row_unit[row] = rx.unit;
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
                Rx::Nfm { channel, .. } => Some(channel.floor()).filter(|f| f.is_finite()),
                Rx::P25(_) => None,
            }));
            if !floors.is_empty() {
                floors.sort_by(f32::total_cmp);
                band.floor_cap = floors[floors.len() / 2] * 2.0;
            }
            if controls.squelch_db() != squelch_db {
                squelch_db = controls.squelch_db();
                for rx in bands.iter_mut().flat_map(|b| &mut b.rxs) {
                    if let Rx::Nfm { channel, .. } = rx {
                        channel.set_squelch(10f32.powf(squelch_db / 10.0));
                    }
                }
            }
            levels.fill(0.0);
            for rx in &bands[tuned].rxs {
                match rx {
                    Rx::Nfm { channel, row } => levels[*row] = channel.snr_db(),
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

        let priority = (0..rows).find(|&c| open[c] && controls.priority(c));
        if let Some(pin) = controls.pinned() {
            current = Some(pin);
        } else if let Some(urgent) = priority.filter(|_| !current.is_some_and(|c| open[c] && controls.priority(c))) {
            current = Some(urgent);
            idle_blocks = 0;
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
                let (snr_db, talkgroup, unit) = match row_rx[c].map(|i| &bands[tuned].rxs[i]) {
                    Some(Rx::P25(rx)) => (rx.channel.snr_db(), rx.talkgroup, rx.unit),
                    Some(Rx::Nfm { channel, .. }) => (channel.snr_db(), None, None),
                    None => (0.0, None, None),
                };
                on_event(Event::Opened {
                    channel: c,
                    snr_db,
                    playing: now_playing == Some(c),
                    talkgroup,
                    unit,
                });
                // A marked channel's calls go to their own place if there is one.
                let marked = cfg.record_marked.as_ref().filter(|_| controls.recorded(c));
                if let Some(dir) = marked.or(cfg.record.as_ref()) {
                    let name = format!(
                        "{}_{}_{}.wav",
                        Local::now().format("%Y%m%d-%H%M%S%.3f"),
                        talkgroup
                            .map_or(e.id(), |tg| format!("TG{tg}"))
                            .replace(|ch: char| !ch.is_alphanumeric() && ch != '.', ""),
                        e.tag.replace(|ch: char| !ch.is_alphanumeric(), "_")
                    );
                    let path = dir.join(name);
                    match wav::Writer::create(&path, AUDIO_RATE) {
                        Ok(w) => recorders[c] = Some((w, path)),
                        Err(err) => eprintln!("scanner: can't record: {err}"),
                    }
                }
            }
            if open[c] {
                open_blocks[c] += 1;
                if let Some((w, _)) = &mut recorders[c] {
                    to_pcm(&audio[c], volume, &mut pcm);
                    w.write(&pcm);
                }
            } else if open_blocks[c] > 0 {
                let recording = recorders[c].take().map(|(w, path)| {
                    w.finish();
                    path
                });
                on_event(Event::Closed {
                    channel: c,
                    secs: open_blocks[c] as f32 * block_secs,
                    unit: row_unit[c].take(),
                    recording,
                });
                open_blocks[c] = 0;
            }
        }
        if now_playing != playing {
            playing = now_playing;
            on_event(Event::Playing(playing));
        }

        if let Some(request) = controls.replay_request.lock().unwrap().take() {
            replay = request
                .and_then(|path| wav::read(&path).ok())
                .map(|samples| (samples, 0));
        }
        if let Some((samples, at)) = &mut replay {
            let end = (*at + audio_len).min(samples.len());
            pcm.clear();
            pcm.extend_from_slice(&samples[*at..end]);
            pcm.resize(audio_len, 0);
            *at = end;
            if end == samples.len() {
                replay = None;
            }
        }
        controls.replaying.store(replay.is_some(), Relaxed);

        if let Some(out) = &mut speaker {
            // Written every block, silence included, to keep the output running.
            let written = match playing {
                _ if controls.muted() => out.write(&silence),
                // A recording being replayed takes the place of live audio.
                _ if replay.is_some() => out.write(&pcm),
                Some(c) => {
                    to_pcm(&audio[c], volume, &mut pcm);
                    out.write(&pcm)
                }
                None => out.write(&silence),
            };
            if written.is_err() {
                break;
            }
        }

        // With more than one band, move on from a quiet one after a while,
        // and from a busy one between transmissions once it has had its
        // turn; a held channel keeps its band tuned.
        if bands.len() > 1 {
            let held = controls.pinned().and_then(|row| row_band[row]);
            let in_call = |rx: &Rx| matches!(rx, Rx::P25(rx) if rx.row.is_some());
            let busy = current.is_some() || open.iter().any(|&o| o) || bands[tuned].rxs.iter().any(in_call);
            band_idle_blocks = if busy { 0 } else { band_idle_blocks + 1 };
            band_blocks += 1;
            let overstayed = band_blocks >= max_stay_blocks && playing.is_none();
            // One channel at a time doesn't wait out the dwell on a channel
            // with nothing on it.
            let signal = |rx: &Rx| match rx {
                Rx::Nfm { channel, .. } => channel.signal_present(),
                Rx::P25(rx) => rx.channel.signal_present(),
            };
            let empty = by_channel && !busy && band_blocks >= check_blocks && !bands[tuned].rxs.iter().any(signal);
            // Bands with only skipped channels on them aren't worth a turn.
            let wanted = |band: &Band| {
                band.rxs.iter().any(|rx| match rx {
                    Rx::Nfm { row, .. } => !controls.skipped(*row),
                    Rx::P25(rx) => rx.fixed.is_none_or(|(row, _)| !controls.skipped(row)),
                })
            };
            let next = match held {
                Some(band) => band,
                None if band_idle_blocks >= dwell_blocks || overstayed || empty => (1..bands.len())
                    .map(|step| (tuned + step) % bands.len())
                    .find(|&band| wanted(&bands[band]))
                    .unwrap_or(tuned),
                None => tuned,
            };
            if next != tuned {
                // Whatever was being received here ends now.
                for rx in &mut bands[tuned].rxs {
                    match rx {
                        Rx::Nfm { channel, row } => {
                            channel.resync();
                            open[*row] = false;
                            row_rx[*row] = None;
                        }
                        Rx::P25(rx) => {
                            if let Some(row) = rx.row {
                                open[row] = false;
                                row_rx[row] = None;
                            }
                            rx.channel.resync();
                            rx.hang_up();
                        }
                    }
                }
                tuned = next;
                current = None;
                band_idle_blocks = 0;
                band_blocks = 0;
                if let Err(e) = input.tune(bands[tuned].center_hz, RETUNE_SETTLE_SECS) {
                    failure = Some(e.to_string());
                    break;
                }
                announce(tuned, &bands, &mut on_event);
            }
        }
    }

    for (w, _) in recorders.into_iter().flatten() {
        w.finish();
    }
    drop(speaker);
    if let Some(failure) = failure {
        return Err(failure);
    }
    if matches!(input, Input::Airspy { .. }) && !controls.stop.load(Relaxed) {
        return Err("the Airspy stopped sending samples (unplugged?)".into());
    }
    Ok(())
}
