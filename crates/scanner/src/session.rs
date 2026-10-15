//! One run of the scanner engine on a background thread, and the live state
//! it publishes for a front end to draw.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::engine::{self, Config, Controls, Event};
use crate::plan::{Kind, Plan};
use chrono::Local;

const LOG_LEN: usize = 200;
/// Spectrum reports kept per receiver: 20 seconds' worth.
pub const SPECTRUM_ROWS: usize = 200;

/// One transmission in the activity log.
pub struct Call {
    /// Counts up from 1 through the session; names the call to other
    /// programs.
    pub id: u64,
    pub time: String,
    /// Which of the channels being scanned it was on. `None` for a call
    /// from before the scan was last restarted, when they may have been
    /// different ones.
    pub channel: Option<usize>,
    /// What to show it as: the channel's name, or "Talkgroup 1234".
    pub name: String,
    /// The channel's frequency in MHz or talkgroup, as text.
    pub label: String,
    pub description: String,
    /// For P25 calls.
    pub talkgroup: Option<u16>,
    /// The calling radio, for P25 calls that say.
    pub unit: Option<u32>,
    /// The call's audio, once it has ended.
    pub recording: Option<PathBuf>,
    pub snr_db: f32,
    /// Length in seconds, once it has ended.
    pub secs: Option<f32>,
    pub played: bool,
}

/// A receiver being scanned with, and where it is tuned.
pub struct TunedReceiver {
    pub name: String,
    /// (which of its bands, of how many, from Hz, to Hz), once it has tuned.
    pub band: Option<(usize, usize, f64, f64)>,
    /// What it has picked up, newest first: see [`Event::Spectrum`].
    pub spectrum: VecDeque<SpectrumRow>,
}

/// One [`Event::Spectrum`] report.
#[derive(Clone)]
pub struct SpectrumRow {
    pub lo_hz: f64,
    pub hi_hz: f64,
    /// Power in equal slices from `lo_hz` to `hi_hz`, in dB of full scale.
    pub db: Vec<f32>,
}

#[derive(Default)]
pub struct Live {
    /// Carrier level over the noise floor per channel, in dB.
    pub levels: Vec<f32>,
    pub open: Vec<bool>,
    pub calls: Vec<u32>,
    pub last_heard: Vec<Option<String>>,
    pub playing: Option<usize>,
    /// With one receiver taking turns between bands: (which, of how many,
    /// from Hz, to Hz).
    pub band: Option<(usize, usize, f64, f64)>,
    /// The receivers being scanned with.
    pub receivers: Vec<TunedReceiver>,
    /// Newest first.
    pub log: VecDeque<Call>,
    /// How many calls there have been.
    pub call_count: u64,
    /// Why the engine stopped, if it did so on its own.
    pub error: Option<String>,
}

pub struct Session {
    pub plan: Arc<Plan>,
    pub controls: Arc<Controls>,
    pub live: Arc<Mutex<Live>>,
    thread: Option<JoinHandle<()>>,
}

impl Live {
    /// With more than one receiver, where each is tuned, for showing:
    /// "Airspy 482.5–490.9 MHz  ·  RTL-SDR band 2 of 3, 153.8–155.9 MHz".
    pub fn tunings(&self) -> Option<String> {
        if self.receivers.len() < 2 {
            return None;
        }
        let tuning = |receiver: &TunedReceiver| {
            let band = match receiver.band {
                None => "starting".to_string(),
                // One channel at a time: each "band" is a single frequency.
                Some((index, count, lo, hi)) if lo == hi && count > 1 => {
                    format!("frequency {} of {count}, {:.4} MHz", index + 1, lo / 1e6)
                }
                Some((_, _, lo, hi)) if lo == hi => format!("{:.4} MHz", lo / 1e6),
                Some((index, count, lo, hi)) if count > 1 => {
                    format!("band {} of {count}, {:.1}–{:.1} MHz", index + 1, lo / 1e6, hi / 1e6)
                }
                Some((_, _, lo, hi)) => format!("{:.1}–{:.1} MHz", lo / 1e6, hi / 1e6),
            };
            format!("{} {band}", receiver.name)
        };
        let tunings: Vec<String> = self.receivers.iter().map(tuning).collect();
        Some(tunings.join("  ·  "))
    }
}

/// The activity log of scans that have ended, to carry into the next one:
/// see [`Session::end`] and [`Session::start_after`].
#[derive(Default)]
pub struct History {
    /// Newest first.
    log: VecDeque<Call>,
    call_count: u64,
}

impl History {
    /// The calls, newest first.
    pub fn log(&self) -> &VecDeque<Call> {
        &self.log
    }
}

impl Session {
    pub fn start(plan: Plan, config: Config, volume: f32, squelch_db: f32) -> Self {
        Self::start_with_audio(plan, config, volume, squelch_db, History::default(), |_| {})
    }

    /// As [`start`](Self::start), with the activity log carrying on from
    /// `history`.
    pub fn start_after(plan: Plan, config: Config, volume: f32, squelch_db: f32, history: History) -> Self {
        Self::start_with_audio(plan, config, volume, squelch_db, history, |_| {})
    }

    /// Stop scanning, releasing the receiver, and hand back the activity
    /// log to start the next scan with.
    pub fn end(mut self) -> History {
        self.controls.stop();
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
        let mut live = self.live.lock().unwrap();
        let mut log = std::mem::take(&mut live.log);
        // The next scan's channels are numbered afresh.
        log.iter_mut().for_each(|call| call.channel = None);
        History {
            log,
            call_count: live.call_count,
        }
    }

    /// As [`start_after`](Self::start_after), also handing `audio` what is
    /// playing as it plays: 16 kHz mono, a millisecond at a time, on the
    /// engine's thread.
    pub fn start_with_audio(
        plan: Plan,
        config: Config,
        volume: f32,
        squelch_db: f32,
        history: History,
        mut audio: impl FnMut(&[i16]) + Send + 'static,
    ) -> Self {
        let plan = Arc::new(plan);
        let n = plan.entries.len();
        let controls = Arc::new(Controls::new(&plan, volume, squelch_db));
        let live = Arc::new(Mutex::new(Live {
            levels: vec![0.0; n],
            open: vec![false; n],
            calls: vec![0; n],
            last_heard: vec![None; n],
            log: history.log,
            call_count: history.call_count,
            ..Live::default()
        }));

        let thread = thread::spawn({
            let (plan, controls, live) = (plan.clone(), controls.clone(), live.clone());
            move || {
                let result = engine::run(&plan, &config, &controls, |event| {
                    // Not worth the lock: this one comes a thousand times a second.
                    if let Event::Audio(pcm) = event {
                        return audio(pcm);
                    }
                    let mut live = live.lock().unwrap();
                    match event {
                        Event::Levels(levels) => live.levels.copy_from_slice(levels),
                        Event::Audio(_) => {}
                        Event::Playing(channel) => {
                            live.playing = channel;
                            // A call picked up partway through still counts as played.
                            if let Some(call) = live.log.iter_mut().find(|c| c.channel == channel && c.secs.is_none()) {
                                call.played = true;
                            }
                        }
                        Event::Receivers(names) => {
                            let receiver = |name: &String| TunedReceiver {
                                name: name.clone(),
                                band: None,
                                spectrum: VecDeque::with_capacity(SPECTRUM_ROWS),
                            };
                            live.receivers = names.iter().map(receiver).collect();
                        }
                        Event::Band {
                            receiver,
                            index,
                            count,
                            lo_hz,
                            hi_hz,
                        } => {
                            if let Some(receiver) = live.receivers.get_mut(receiver) {
                                receiver.band = Some((index, count, lo_hz, hi_hz));
                            }
                            if live.receivers.len() < 2 {
                                live.band = (count > 1).then_some((index, count, lo_hz, hi_hz));
                            }
                        }
                        Event::Spectrum {
                            receiver,
                            lo_hz,
                            hi_hz,
                            db,
                        } => {
                            if let Some(receiver) = live.receivers.get_mut(receiver) {
                                let rows = &mut receiver.spectrum;
                                // The oldest row's memory is reused for the newest.
                                let mut row = match rows.len() {
                                    SPECTRUM_ROWS => rows.pop_back().unwrap(),
                                    _ => SpectrumRow {
                                        lo_hz,
                                        hi_hz,
                                        db: Vec::new(),
                                    },
                                };
                                (row.lo_hz, row.hi_hz) = (lo_hz, hi_hz);
                                row.db.clear();
                                row.db.extend_from_slice(db);
                                rows.push_front(row);
                            }
                        }
                        Event::Opened {
                            channel,
                            snr_db,
                            playing,
                            talkgroup,
                            unit,
                        } => {
                            let time = Local::now().format("%H:%M:%S").to_string();
                            live.open[channel] = true;
                            live.calls[channel] += 1;
                            live.last_heard[channel] = Some(time.clone());
                            live.call_count += 1;
                            let id = live.call_count;
                            let e = &plan.entries[channel];
                            // A call on a talkgroup that isn't listed goes
                            // by its number.
                            let (name, label) = match (&e.kind, talkgroup) {
                                (Kind::OtherTalkgroups { .. }, Some(tg)) => {
                                    (format!("Talkgroup {tg}"), format!("TG {tg}"))
                                }
                                (_, _) => (e.tag.clone(), e.id()),
                            };
                            live.log.push_front(Call {
                                id,
                                time,
                                channel: Some(channel),
                                name,
                                label,
                                description: e.desc.clone(),
                                talkgroup,
                                unit,
                                recording: None,
                                snr_db,
                                secs: None,
                                played: playing,
                            });
                            live.log.truncate(LOG_LEN);
                        }
                        Event::Closed {
                            channel,
                            secs,
                            unit,
                            recording,
                        } => {
                            live.open[channel] = false;
                            if let Some(call) = live
                                .log
                                .iter_mut()
                                .find(|c| c.channel == Some(channel) && c.secs.is_none())
                            {
                                call.secs = Some(secs);
                                call.unit = unit.or(call.unit);
                                call.recording = recording;
                            }
                        }
                    }
                });
                let mut live = live.lock().unwrap();
                live.playing = None;
                live.open.fill(false);
                live.error = Some(result.err().unwrap_or_else(|| "Receiver stopped.".into()));
            }
        });
        Self {
            plan,
            controls,
            live,
            thread: Some(thread),
        }
    }
}

impl Drop for Session {
    /// Stops the engine and waits for it to release the Airspy.
    fn drop(&mut self) {
        self.controls.stop();
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Source;

    #[test]
    fn carries_the_log_into_the_next_scan() {
        let plan = || {
            Plan::parse("155.0, 114.8, n, Test, A test channel\n", "test", None)
                .unwrap()
                .0
        };
        // No such receiver, so the scans end at once; the log is what matters.
        let config = || Config {
            source: Source::Kind("teapot".into()),
            audio: false,
            ..Config::default()
        };
        let first = Session::start(plan(), config(), 3.0, 6.0);
        {
            let mut live = first.live.lock().unwrap();
            live.call_count = 7;
            live.log.push_front(Call {
                id: 7,
                time: "12:00:00".into(),
                channel: Some(0),
                name: "Test".into(),
                label: "155.0000".into(),
                description: "A test channel".into(),
                talkgroup: None,
                unit: None,
                recording: None,
                snr_db: 20.0,
                secs: Some(3.0),
                played: true,
            });
        }
        let history = first.end();
        assert_eq!(history.log().len(), 1);

        let second = Session::start_after(plan(), config(), 3.0, 6.0, history);
        let live = second.live.lock().unwrap();
        // Still there, no longer tied to a channel of this scan, and the
        // next call will be numbered after it.
        assert_eq!(live.log[0].name, "Test");
        assert_eq!(live.log[0].channel, None);
        assert_eq!(live.call_count, 7);
    }
}
