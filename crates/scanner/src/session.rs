//! One run of the scanner engine on a background thread, and the live state
//! it publishes for a front end to draw.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::engine::{self, Config, Controls, Event};
use crate::plan::Plan;
use chrono::Local;

const LOG_LEN: usize = 200;

/// One transmission in the activity log.
pub struct Call {
    /// Counts up from 1 through the session; names the call to other
    /// programs.
    pub id: u64,
    pub time: String,
    pub channel: usize,
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

#[derive(Default)]
pub struct Live {
    /// Carrier level over the noise floor per channel, in dB.
    pub levels: Vec<f32>,
    pub open: Vec<bool>,
    pub calls: Vec<u32>,
    pub last_heard: Vec<Option<String>>,
    pub playing: Option<usize>,
    /// When taking turns between bands: (which, of how many, from Hz, to Hz).
    pub band: Option<(usize, usize, f64, f64)>,
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

impl Session {
    pub fn start(plan: Plan, config: Config, volume: f32, squelch_db: f32) -> Self {
        Self::start_with_audio(plan, config, volume, squelch_db, |_| {})
    }

    /// As [`start`](Self::start), also handing `audio` what is playing as it
    /// plays: 16 kHz mono, a millisecond at a time, on the engine's thread.
    pub fn start_with_audio(
        plan: Plan,
        config: Config,
        volume: f32,
        squelch_db: f32,
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
                            if let Some(call) = live
                                .log
                                .iter_mut()
                                .find(|c| Some(c.channel) == channel && c.secs.is_none())
                            {
                                call.played = true;
                            }
                        }
                        Event::Band {
                            index,
                            count,
                            lo_hz,
                            hi_hz,
                        } => live.band = (count > 1).then_some((index, count, lo_hz, hi_hz)),
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
                            live.log.push_front(Call {
                                id,
                                time,
                                channel,
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
                            if let Some(call) = live.log.iter_mut().find(|c| c.channel == channel && c.secs.is_none()) {
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
