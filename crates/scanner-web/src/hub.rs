//! The one scanner every browser shares: its settings, its scan, and what
//! each listener is sent.

use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use scanner::db::Db;
use scanner::engine::{self, Config, Radio, ScanMode};
use scanner::plan::{Kind, Plan};
use scanner::session::Session;
use scanner::settings::{AUTO_DEVICE, Settings};
use scanner_proto::{
    AUDIO_RATE, Band, Call, Channel, ChannelState, Choice, ClientMessage, Options, ServerMessage, Status, System,
};

/// Messages that may wait for a slow listener before it starts missing them.
pub const CLIENT_QUEUE: usize = 256;
/// Audio is sent in pieces this long.
const AUDIO_FRAME_SAMPLES: usize = AUDIO_RATE as usize / 25;
/// Calls kept in the activity log sent to listeners.
const LOG_LEN: usize = 100;

/// What a listener is sent.
#[derive(Clone)]
pub enum Outgoing {
    /// A [`ServerMessage`] as JSON.
    Text(Arc<str>),
    /// Audio that is playing, as little-endian 16-bit samples.
    Audio(Arc<[u8]>),
}

/// The listeners connected. Kept apart from the [`Hub`] so that audio,
/// which comes from the scan's own thread, never waits on it.
#[derive(Clone, Default)]
pub struct Clients(Arc<Mutex<Vec<SyncSender<Outgoing>>>>);

impl Clients {
    pub fn add(&self, client: SyncSender<Outgoing>) {
        self.0.lock().unwrap().push(client);
    }

    /// Send to everyone, dropping it for anyone too far behind and
    /// forgetting anyone who has gone.
    fn send(&self, message: Outgoing) {
        self.0
            .lock()
            .unwrap()
            .retain(|client| match client.try_send(message.clone()) {
                Ok(()) | Err(TrySendError::Full(_)) => true,
                Err(TrySendError::Disconnected(_)) => false,
            });
    }
}

/// The scan modes: what each is known as and called.
const SCAN_MODES: [(ScanMode, &str); 3] = [
    (ScanMode::OneBand, "One band"),
    (ScanMode::HopBands, "Hop between bands"),
    (ScanMode::Channels, "One channel at a time"),
];

pub struct Hub {
    db: Db,
    /// Where recordings that are kept go: beside the channel database.
    kept_recordings: PathBuf,
    settings: Settings,
    /// The receiver that was found when last looked for.
    radio: Option<Radio>,
    session: Option<Session>,
    /// Why nothing is being scanned, when nothing is.
    problem: Option<String>,
    /// Also play through this computer's speakers.
    local_audio: bool,
    clients: Clients,
}

impl Hub {
    pub fn new(db: Db, kept_recordings: PathBuf, local_audio: bool, clients: Clients) -> Self {
        let mut settings = Settings::load(Some(&db));
        let systems = db.systems().unwrap_or_default();
        settings.selected.retain(|id| systems.iter().any(|s| s.id == *id));
        if settings.selected.is_empty() {
            settings.selected = systems.first().map(|s| s.id).into_iter().collect();
        }
        Self {
            db,
            kept_recordings,
            settings,
            radio: None,
            session: None,
            problem: None,
            local_audio,
            clients,
        }
    }

    pub fn problem(&self) -> Option<String> {
        let stopped = self.session.as_ref().and_then(|s| s.live.lock().unwrap().error.clone());
        self.problem.clone().or(stopped)
    }

    /// Look for the kind of receiver the settings ask for. It can't be
    /// asked what it offers while it is in use, so this stops any scan.
    pub fn find_receiver(&mut self) {
        self.session = None;
        let source = Config {
            source: self.settings.source(),
            ..Config::default()
        };
        match engine::probe(&source) {
            Ok(radio) => {
                self.radio = Some(radio);
                self.problem = None;
            }
            Err(e) => {
                self.radio = None;
                self.problem = Some(e);
            }
        }
    }

    /// The sample rate to run at: the chosen one if the receiver offers it,
    /// otherwise its fastest.
    fn rate(&self) -> Option<u32> {
        let rates = &self.radio.as_ref()?.rates;
        let chosen = rates.iter().find(|&&rate| rate == self.settings.sample_rate);
        chosen.or(rates.iter().max()).copied()
    }

    fn usable_span_hz(&self) -> Option<f64> {
        Some(self.radio.as_ref()?.usable_span_hz(self.rate()?))
    }

    /// Where recordings go: beside the channel database if they are being
    /// kept, otherwise somewhere temporary (they are still needed, to
    /// replay calls).
    fn recordings_dir(&self) -> PathBuf {
        if self.settings.save_recordings {
            self.kept_recordings.clone()
        } else {
            std::env::temp_dir().join(format!("airspy-scanner-replay-{}", std::process::id()))
        }
    }

    /// Scan the selected systems.
    pub fn start(&mut self) {
        // Only one scan can hold the receiver: stop the old one first.
        self.session = None;
        if self.radio.is_none() {
            return;
        }
        self.problem = None;
        if self.settings.selected.is_empty() {
            self.problem = Some("No system is selected.".into());
            return;
        }
        match self.db.plan(&self.settings.selected) {
            Ok(plan) => {
                let config = Config {
                    source: self.settings.source(),
                    rate: self.rate(),
                    gain: self.settings.gain,
                    ppm: self.settings.ppm,
                    bias_tee: self.settings.bias_tee,
                    scan: self.settings.scan,
                    dwell_secs: self.settings.dwell_secs,
                    max_stay_secs: self.settings.max_stay_secs,
                    record: Some(self.recordings_dir()),
                    record_marked: Some(self.kept_recordings.clone()),
                    audio: self.local_audio,
                    ..Config::default()
                };
                let clients = self.clients.clone();
                // Audio arrives a millisecond at a time; listeners get it in
                // larger pieces.
                let mut frame = Vec::with_capacity(AUDIO_FRAME_SAMPLES * 2);
                let audio = move |pcm: &[i16]| {
                    frame.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
                    if frame.len() >= AUDIO_FRAME_SAMPLES * 2 {
                        clients.send(Outgoing::Audio(Arc::from(frame.as_slice())));
                        frame.clear();
                    }
                };
                let (volume, squelch_db) = (self.settings.volume, self.settings.squelch_db);
                self.session = Some(Session::start_with_audio(plan, config, volume, squelch_db, audio));
            }
            Err(e) => self.problem = Some(e),
        }
    }

    /// Stop scanning and clear up, before the program exits.
    pub fn shut_down(&mut self) {
        self.session = None;
        if !self.settings.save_recordings {
            std::fs::remove_dir_all(self.recordings_dir()).ok();
        }
    }

    // ── What listeners are sent ─────────────────────────────────────────

    fn broadcast(&self, message: ServerMessage) {
        self.clients.send(Outgoing::Text(Arc::from(message.to_json())));
    }

    fn systems(&self) -> ServerMessage {
        let systems = self.db.systems().unwrap_or_default();
        ServerMessage::Systems {
            systems: systems
                .into_iter()
                .map(|s| System {
                    selected: self.settings.selected.contains(&s.id),
                    id: s.id,
                    name: s.name,
                    location: s.location,
                    channels: s.channels,
                    lo_hz: s.lo_hz,
                    hi_hz: s.hi_hz,
                })
                .collect(),
        }
    }

    fn plan(&self) -> Option<&Plan> {
        self.session.as_ref().map(|s| &*s.plan)
    }

    fn channels(&self) -> ServerMessage {
        let entries = self.plan().map_or(&[][..], |plan| &plan.entries);
        ServerMessage::Channels {
            channels: entries
                .iter()
                .map(|e| Channel {
                    label: e.id(),
                    name: e.tag.clone(),
                    description: e.desc.clone(),
                })
                .collect(),
        }
    }

    fn options(&self) -> ServerMessage {
        let s = &self.settings;
        let choice = |id: &str, name: &str| Choice {
            id: id.to_string(),
            name: name.to_string(),
        };
        let mut devices = vec![choice(AUTO_DEVICE, "Automatic")];
        devices.extend(engine::drivers().iter().map(|d| choice(d.id(), d.name())));
        ServerMessage::Options(Options {
            volume: s.volume,
            squelch_db: s.squelch_db,
            muted: self.session.as_ref().is_some_and(|session| session.controls.muted()),
            gain: s.gain,
            ppm: s.ppm,
            bias_tee: s.bias_tee,
            scan_mode: s.scan.id().to_string(),
            scan_modes: SCAN_MODES.iter().map(|(mode, name)| choice(mode.id(), name)).collect(),
            device: s.device.clone(),
            devices,
            receiver: self.radio.as_ref().map(|radio| radio.name.clone()),
            sample_rate: self.rate(),
            sample_rates: self.radio.as_ref().map_or(Vec::new(), |radio| radio.rates.clone()),
            span_hz: self.usable_span_hz(),
        })
    }

    fn status(&self) -> ServerMessage {
        let Some(session) = &self.session else {
            return ServerMessage::Status(Status {
                problem: Some(self.problem.clone().unwrap_or_else(|| "Not scanning.".into())),
                ..Status::default()
            });
        };
        let (live, controls) = (session.live.lock().unwrap(), &session.controls);
        let entries = &session.plan.entries;
        ServerMessage::Status(Status {
            channels: (0..entries.len())
                .map(|c| ChannelState {
                    level_db: live.levels[c],
                    open: live.open[c],
                    calls: live.calls[c],
                    last_heard: live.last_heard[c].clone(),
                    skipped: controls.skipped(c),
                    priority: controls.priority(c),
                    recorded: controls.recorded(c),
                })
                .collect(),
            playing: live.playing,
            held: controls.pinned(),
            band: live.band.map(|(index, count, lo_hz, hi_hz)| Band {
                index,
                count,
                lo_hz,
                hi_hz,
            }),
            replaying: controls.replaying(),
            log: live
                .log
                .iter()
                .take(LOG_LEN)
                .map(|call| Call {
                    id: call.id,
                    time: call.time.clone(),
                    channel: call.channel,
                    name: match (&entries[call.channel].kind, call.talkgroup) {
                        (Kind::OtherTalkgroups { .. }, Some(talkgroup)) => format!("Talkgroup {talkgroup}"),
                        (_, _) => entries[call.channel].tag.clone(),
                    },
                    unit: call.unit,
                    snr_db: call.snr_db,
                    secs: call.secs,
                    played: call.played,
                    recorded: call.recording.is_some(),
                })
                .collect(),
            problem: live.error.clone(),
        })
    }

    /// Everything a listener needs on connecting, in the order to send it.
    pub fn greeting(&self) -> Vec<Outgoing> {
        [self.systems(), self.channels(), self.options(), self.status()]
            .iter()
            .map(|message| Outgoing::Text(Arc::from(message.to_json())))
            .collect()
    }

    pub fn send_status(&self) {
        self.broadcast(self.status());
    }

    /// Tell everyone about a change to what is being scanned.
    fn send_everything(&self) {
        for message in [self.systems(), self.channels(), self.options(), self.status()] {
            self.broadcast(message);
        }
    }

    // ── What listeners ask for ──────────────────────────────────────────

    /// Change settings, remember them, and restart the scan if `restart`
    /// (for the ones the receiver only reads when it starts).
    fn update(&mut self, restart: bool, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        self.settings.save(&self.db);
        if restart {
            self.start();
            self.send_everything();
        } else {
            self.broadcast(self.options());
        }
    }

    /// Remember a channel's mark in the database as well as applying it.
    fn mark(&self, channel: usize, set: fn(&Db, i64, bool) -> Result<(), String>, on: bool) {
        let id = self
            .plan()
            .and_then(|plan| plan.entries.get(channel))
            .and_then(|e| e.channel_id);
        if let Some(id) = id {
            set(&self.db, id, on).ok();
        }
    }

    /// Whether `channel` is one of the channels being scanned.
    fn has_channel(&self, channel: usize) -> bool {
        self.plan().is_some_and(|plan| channel < plan.entries.len())
    }

    pub fn handle(&mut self, message: ClientMessage) {
        match message {
            ClientMessage::ToggleSystem { id } => self.toggle_system(id),
            ClientMessage::SetVolume { volume } => {
                self.update(false, |s| s.volume = volume.clamp(0.5, 10.0));
                if let Some(session) = &self.session {
                    session.controls.set_volume(self.settings.volume);
                }
            }
            ClientMessage::SetSquelch { db } => {
                self.update(false, |s| s.squelch_db = db.clamp(3.0, 30.0));
                if let Some(session) = &self.session {
                    session.controls.set_squelch_db(self.settings.squelch_db);
                }
            }
            ClientMessage::SetMuted { muted } => {
                if let Some(session) = &self.session {
                    session.controls.set_muted(muted);
                }
                self.broadcast(self.options());
            }
            ClientMessage::SetGain { gain } => self.update(true, |s| s.gain = gain.min(21)),
            ClientMessage::SetPpm { ppm } => self.update(true, |s| s.ppm = ppm.clamp(-200, 200)),
            ClientMessage::SetBiasTee { on } => self.update(true, |s| s.bias_tee = on),
            ClientMessage::SetScanMode { id } => {
                if let Some(mode) = ScanMode::from_id(&id) {
                    self.update(true, |s| s.scan = mode);
                }
            }
            ClientMessage::SetSampleRate { rate } => self.update(true, |s| s.sample_rate = rate),
            ClientMessage::SetDevice { id } => {
                let known = id == AUTO_DEVICE || engine::drivers().iter().any(|d| d.id() == id);
                if known {
                    self.settings.device = id;
                    self.settings.save(&self.db);
                    self.find_receiver();
                    self.start();
                    self.send_everything();
                }
            }
            ClientMessage::Hold { channel } => {
                if let (Some(session), true) = (&self.session, channel.is_none_or(|c| self.has_channel(c))) {
                    session.controls.set_pinned(channel);
                    if let Some(channel) = channel {
                        session.controls.set_skipped(channel, false);
                    }
                }
            }
            ClientMessage::Skip { channel, on } if self.has_channel(channel) => {
                if let Some(session) = &self.session {
                    session.controls.set_skipped(channel, on);
                    if on && session.controls.pinned() == Some(channel) {
                        session.controls.set_pinned(None);
                    }
                }
                self.mark(channel, Db::set_skip, on);
            }
            ClientMessage::Priority { channel, on } if self.has_channel(channel) => {
                if let Some(session) = &self.session {
                    session.controls.set_priority(channel, on);
                }
                self.mark(channel, Db::set_priority, on);
            }
            ClientMessage::Record { channel, on } if self.has_channel(channel) => {
                if let Some(session) = &self.session {
                    // A channel worth recording is one not to miss.
                    session.controls.set_recorded(channel, on);
                    if on {
                        session.controls.set_priority(channel, true);
                        session.controls.set_skipped(channel, false);
                    }
                }
                self.mark(channel, Db::set_record, on);
            }
            ClientMessage::Skip { .. } | ClientMessage::Priority { .. } | ClientMessage::Record { .. } => {}
            ClientMessage::Replay { call } => {
                if let Some(session) = &self.session {
                    let recording = session
                        .live
                        .lock()
                        .unwrap()
                        .log
                        .iter()
                        .find(|c| c.id == call)
                        .and_then(|c| c.recording.clone());
                    if let Some(recording) = recording {
                        session.controls.replay(recording);
                    }
                }
            }
            ClientMessage::StopReplay => {
                if let Some(session) = &self.session {
                    session.controls.stop_replay();
                }
            }
        }
        // So whoever asked sees the result at once.
        self.send_status();
    }

    /// Add a system to the scan or take it out. In one-band mode, systems
    /// too far apart to share a tuning replace the selection instead.
    fn toggle_system(&mut self, id: i64) {
        let selected = &mut self.settings.selected;
        if let Some(i) = selected.iter().position(|s| *s == id) {
            selected.remove(i);
        } else if self
            .db
            .systems()
            .is_ok_and(|systems| systems.iter().any(|s| s.id == id))
        {
            let mut both = selected.clone();
            both.push(id);
            let fits = self.db.plan(&both).is_ok_and(|plan| {
                let (lo, hi, _) = plan.span();
                self.usable_span_hz().is_some_and(|span| hi - lo <= span)
            });
            self.settings.selected = if fits || self.settings.scan != ScanMode::OneBand {
                both
            } else {
                vec![id]
            };
        }
        self.update(true, |_| {});
    }
}
