//! The app's one view: what is playing, and tabs for channels, activity,
//! systems and settings.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use gpui::{Context, Div, FontWeight, SharedString, Stateful, Window, div, prelude::*, px, relative, rgb};
use gpui_mobile::android::jni;
use scanner::db::{Db, System};
use scanner::engine::{self, Config, ScanMode, Source};
use scanner::plan::Kind;
use scanner::session::{Live, Session};

use crate::usb::{self, Opened};

// Matches `gpui_surface` in the Android resources, so the splash screen
// hands over to the first frame without a flash.
const BG: u32 = 0x121318;
const PANEL: u32 = 0x1c2027;
const RAISED: u32 = 0x2a303a;
const BORDER: u32 = 0x313743;
const TEXT: u32 = 0xe6e9ee;
const MUTED: u32 = 0x8b93a1;
const ACCENT: u32 = 0x4cc38a;
const HOLD: u32 = 0xe0a84c;
const PRIORITY: u32 = 0x6aa6ff;
const ERROR: u32 = 0xe5645c;

/// Signal level that fills a channel's meter, in dB over the noise floor.
const METER_FULL_DB: f32 = 40.0;
const REFRESH: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Channels,
    Activity,
    Systems,
    Settings,
}

const TABS: [(Tab, &str); 4] = [
    (Tab::Channels, "Channels"),
    (Tab::Activity, "Activity"),
    (Tab::Systems, "Systems"),
    (Tab::Settings, "Settings"),
];

/// The scan modes: what each is saved as and called.
const SCAN_MODES: [(ScanMode, &str, &str); 3] = [
    (ScanMode::OneBand, "band", "One band"),
    (ScanMode::HopBands, "hop", "Hop bands"),
    (ScanMode::Channels, "channel", "One channel"),
];

pub struct ScannerApp {
    /// The app's private storage.
    data_dir: PathBuf,
    /// `None` if the channel database couldn't be opened.
    db: Option<Db>,
    systems: Vec<System>,
    /// Systems being scanned together, in the order they were picked.
    selected: Vec<i64>,
    /// The open connection to the Airspy, as Android's file descriptor.
    airspy: Option<i32>,
    /// The sample rates the Airspy offers, once it has been asked.
    rates: Vec<u32>,
    session: Option<Session>,
    /// Why nothing is being scanned, when nothing is.
    status: String,
    tab: Tab,
    /// The channel whose buttons are showing.
    expanded: Option<usize>,
    active_only: bool,
    volume: f32,
    squelch_db: f32,
    gain: u8,
    scan: ScanMode,
    /// Chosen sample rate; 0 for the slowest, which suits a phone.
    sample_rate: u32,
}

impl ScannerApp {
    pub fn new(data_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let (db, status) = match Db::open(&data_dir.join("channels.db")) {
            Ok(db) => (Some(db), String::new()),
            Err(e) => (None, e),
        };
        fn saved<T: FromStr>(db: Option<&Db>, key: &str, default: T) -> T {
            db.and_then(|db| db.setting(key))
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        let defaults = Config::default();
        let scan_name = saved(db.as_ref(), "scan_mode", String::new());
        let mut app = Self {
            systems: Vec::new(),
            selected: saved(db.as_ref(), "selected", String::new())
                .split(',')
                .filter_map(|id| id.parse().ok())
                .collect(),
            airspy: None,
            rates: Vec::new(),
            session: None,
            status,
            tab: Tab::Channels,
            expanded: None,
            active_only: false,
            volume: saved(db.as_ref(), "volume", 3.0),
            squelch_db: saved(db.as_ref(), "squelch_db", 6.0),
            gain: saved(db.as_ref(), "gain", defaults.gain),
            scan: SCAN_MODES
                .iter()
                .find(|m| m.1 == scan_name)
                .map_or(defaults.scan, |m| m.0),
            sample_rate: saved(db.as_ref(), "sample_rate", 0),
            data_dir,
            db,
        };
        app.reload_systems();
        if app.selected.is_empty() {
            app.selected = app.systems.first().map(|s| s.id).into_iter().collect();
        }
        // Recordings only exist so calls can be replayed; start clean.
        std::fs::remove_dir_all(app.recordings_dir()).ok();
        app.connect();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        app
    }

    fn recordings_dir(&self) -> PathBuf {
        self.data_dir.join("recordings")
    }

    fn reload_systems(&mut self) {
        if let Some(db) = &self.db {
            match db.systems() {
                Ok(systems) => self.systems = systems,
                Err(e) => self.status = e,
            }
        }
        self.selected.retain(|id| self.systems.iter().any(|s| s.id == *id));
    }

    fn save(&self, key: &str, value: impl ToString) {
        if let Some(db) = &self.db {
            db.set_setting(key, &value.to_string()).ok();
        }
    }

    /// The sample rate to run at: the chosen one if this Airspy offers it,
    /// otherwise its slowest.
    fn rate(&self) -> Option<u32> {
        let chosen = self.rates.iter().find(|&&rate| rate == self.sample_rate);
        chosen.or(self.rates.iter().min()).copied()
    }

    /// Find the Airspy, asking Android for it, and start scanning.
    fn connect(&mut self) {
        self.session = None;
        self.airspy = None;
        self.status = match usb::open() {
            Opened::Fd(fd) => {
                let source = Config {
                    source: Source::AirspyFd(fd),
                    ..Config::default()
                };
                match engine::sample_rates(&source) {
                    Ok(rates) => {
                        self.rates = rates;
                        self.airspy = Some(fd);
                        String::new()
                    }
                    Err(e) => e,
                }
            }
            Opened::NoDevice => "No Airspy found. Plug one in with a USB OTG adapter.".into(),
            Opened::PermissionRequested => "Allow the app to use the Airspy, then tap Connect.".into(),
            Opened::Failed(e) => e,
        };
        self.start();
    }

    /// Scan the selected systems, if there is an Airspy to scan with.
    fn start(&mut self) {
        // Only one receiver can hold the Airspy: stop the old one first.
        self.session = None;
        self.expanded = None;
        let (Some(fd), Some(db)) = (self.airspy, &self.db) else {
            return;
        };
        if self.selected.is_empty() {
            self.status = "Pick a system to scan in the Systems tab.".into();
            return;
        }
        match db.plan(&self.selected) {
            Ok(plan) => {
                let config = Config {
                    source: Source::AirspyFd(fd),
                    rate: self.rate(),
                    gain: self.gain,
                    scan: self.scan,
                    // Always recorded, so calls can be replayed from the log.
                    record: Some(self.recordings_dir()),
                    ..Config::default()
                };
                self.status.clear();
                self.session = Some(Session::start(plan, config, self.volume, self.squelch_db));
            }
            Err(e) => self.status = e,
        }
    }

    /// Add a system to the scan or take it out. In one-band mode, systems
    /// too far apart to share a tuning replace the selection instead.
    fn toggle_system(&mut self, id: i64) {
        if let Some(i) = self.selected.iter().position(|s| *s == id) {
            self.selected.remove(i);
        } else {
            let mut both = self.selected.clone();
            both.push(id);
            let fits = self.db.as_ref().and_then(|db| db.plan(&both).ok()).is_some_and(|plan| {
                let (lo, hi, _) = plan.span();
                self.rate().is_some_and(|rate| hi - lo <= engine::usable_span_hz(rate))
            });
            self.selected = if fits || self.scan != ScanMode::OneBand {
                both
            } else {
                vec![id]
            };
        }
        let ids: Vec<String> = self.selected.iter().map(i64::to_string).collect();
        self.save("selected", ids.join(","));
        self.start();
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.5, 10.0);
        self.save("volume", self.volume);
        if let Some(s) = &self.session {
            s.controls.set_volume(self.volume);
        }
    }

    fn set_squelch(&mut self, db: f32) {
        self.squelch_db = db.clamp(3.0, 30.0);
        self.save("squelch_db", self.squelch_db);
        if let Some(s) = &self.session {
            s.controls.set_squelch_db(self.squelch_db);
        }
    }

    // ── Pieces of the screen ────────────────────────────────────────────

    /// What is playing, or why nothing is.
    fn now_playing(&self, live: Option<&Live>, cx: &mut Context<Self>) -> Div {
        let replaying = self.session.as_ref().is_some_and(|s| s.controls.replaying());
        let stopped = live.and_then(|live| live.error.clone());
        let (title, detail, color) = match (&self.session, live) {
            (Some(_), Some(_)) if stopped.is_some() => {
                ("Receiver stopped".to_string(), stopped.clone().unwrap(), ERROR)
            }
            (Some(_), Some(_)) if replaying => ("Replaying".into(), "A recorded call".into(), PRIORITY),
            (Some(session), Some(live)) => match (live.playing, session.controls.pinned()) {
                (Some(c), _) => {
                    let e = &session.plan.entries[c];
                    (e.tag.clone(), format!("{} · {}", e.desc, e.id()), ACCENT)
                }
                (None, Some(c)) => {
                    let e = &session.plan.entries[c];
                    (e.tag.clone(), format!("Holding on {}", e.id()), HOLD)
                }
                (None, None) => {
                    let detail = match live.band {
                        Some((index, count, lo, hi)) if lo == hi => {
                            format!("Frequency {} of {} · {:.4} MHz", index + 1, count, lo / 1e6)
                        }
                        Some((index, count, lo, hi)) => {
                            format!("Band {} of {} · {:.1}–{:.1} MHz", index + 1, count, lo / 1e6, hi / 1e6)
                        }
                        None => format!("{} channels", session.plan.entries.len()),
                    };
                    ("Scanning".into(), detail, MUTED)
                }
            },
            _ => ("Not scanning".into(), self.status.clone(), MUTED),
        };
        let needs_connect = self.session.is_none() && self.airspy.is_none() || stopped.is_some();
        div()
            .flex_none()
            .px_4()
            .pb_3()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(div().text_xs().text_color(rgb(MUTED)).child("AIRSPY SCANNER"))
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(color))
                    .truncate()
                    .child(title),
            )
            .child(div().text_sm().text_color(rgb(MUTED)).child(detail))
            .when(needs_connect, |top| {
                top.child(
                    div()
                        .pt_2()
                        .flex()
                        .child(
                            button("connect", "Connect", true, ACCENT).on_click(cx.listener(|this, _, _, cx| {
                                this.connect();
                                cx.notify();
                            })),
                        ),
                )
            })
    }

    fn channels(&self, session: &Session, live: &Live, cx: &mut Context<Self>) -> Stateful<Div> {
        let active_only = self.active_only;
        let shown: Vec<usize> = (0..session.plan.entries.len())
            .filter(|&c| !active_only || live.open[c])
            .collect();
        scroller("channels")
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .child(heading("Channels"))
                    .child(
                        button("active-only", "Active only", active_only, ACCENT).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.active_only = !active_only;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .when(shown.is_empty(), |list| {
                list.child(note("Nothing is transmitting right now."))
            })
            .children(shown.into_iter().map(|c| self.channel_row(c, session, live, cx)))
    }

    fn channel_row(&self, c: usize, session: &Session, live: &Live, cx: &mut Context<Self>) -> Div {
        let e = &session.plan.entries[c];
        let controls = &session.controls;
        let (skipped, pinned) = (controls.skipped(c), controls.pinned() == Some(c));
        let (priority, recorded) = (controls.priority(c), controls.recorded(c));
        let (open, playing) = (live.open[c], live.playing == Some(c));
        let expanded = self.expanded == Some(c);
        let level = (live.levels[c] / METER_FULL_DB).clamp(0.0, 1.0);
        let color = if playing {
            ACCENT
        } else if open {
            TEXT
        } else {
            MUTED
        };
        // Marks that apply to the channel, shown beside its name.
        let marks: String = [
            (pinned, " ·hold"),
            (priority, " ·pri"),
            (recorded, " ·rec"),
            (skipped, " ·skip"),
        ]
        .iter()
        .filter(|m| m.0)
        .map(|m| m.1)
        .collect();
        let channel_id = e.channel_id;

        let summary =
            div()
                .id(("channel", c))
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.expanded = (!expanded).then_some(c);
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_none()
                        .size_2()
                        .rounded_full()
                        .bg(rgb(if open { ACCENT } else { BORDER })),
                )
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .truncate()
                                .text_color(rgb(color))
                                .child(format!("{}{marks}", e.tag)),
                        )
                        .child(div().text_sm().truncate().text_color(rgb(MUTED)).child(format!(
                            "{} · {}",
                            e.id(),
                            e.desc
                        ))),
                )
                .child(
                    div().flex_none().w(px(44.)).h_1().rounded_full().bg(rgb(RAISED)).child(
                        div()
                            .h_full()
                            .w(relative(level))
                            .rounded_full()
                            .bg(rgb(if open { ACCENT } else { MUTED })),
                    ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(32.))
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(match live.calls[c] {
                            0 => String::new(),
                            n => format!("×{n}"),
                        }),
                );

        let row = div()
            .when(playing || expanded, |row| row.bg(rgb(PANEL)))
            .when(skipped, |row| row.opacity(0.5))
            .child(summary);
        if !expanded {
            return row;
        }
        // Remember a channel's marks in the database as well as applying them.
        let remember = move |this: &Self, set: fn(&Db, i64, bool) -> Result<(), String>, on: bool| {
            if let (Some(db), Some(channel)) = (&this.db, channel_id) {
                set(db, channel, on).ok();
            }
        };
        row.child(
            div()
                .flex()
                .gap_2()
                .px_4()
                .pb_3()
                .child(
                    button(("hold", c), "Hold", pinned, HOLD).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(s) = &this.session {
                            s.controls.set_pinned((!pinned).then_some(c));
                            s.controls.set_skipped(c, false);
                        }
                        cx.notify();
                    })),
                )
                .child(
                    button(("skip", c), "Skip", skipped, ERROR).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(s) = &this.session {
                            s.controls.set_skipped(c, !skipped);
                            if pinned {
                                s.controls.set_pinned(None);
                            }
                        }
                        remember(this, Db::set_skip, !skipped);
                        cx.notify();
                    })),
                )
                .child(
                    button(("priority", c), "Priority", priority, PRIORITY).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Some(s) = &this.session {
                                s.controls.set_priority(c, !priority);
                            }
                            remember(this, Db::set_priority, !priority);
                            cx.notify();
                        },
                    )),
                )
                .child(button(("record", c), "Record", recorded, ERROR).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(s) = &this.session {
                            // A channel worth recording is one not to miss.
                            s.controls.set_recorded(c, !recorded);
                            if !recorded {
                                s.controls.set_priority(c, true);
                            }
                        }
                        remember(this, Db::set_record, !recorded);
                        cx.notify();
                    },
                ))),
        )
    }

    fn activity(&self, session: &Session, live: &Live, cx: &mut Context<Self>) -> Stateful<Div> {
        let replaying = session.controls.replaying();
        scroller("activity")
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .child(heading("Activity"))
                    .when(replaying, |title| {
                        title.child(
                            button("stop-replay", "Stop replay", true, PRIORITY).on_click(cx.listener(
                                |this, _, _, cx| {
                                    if let Some(s) = &this.session {
                                        s.controls.stop_replay();
                                    }
                                    cx.notify();
                                },
                            )),
                        )
                    }),
            )
            .when(live.log.is_empty(), |log| log.child(note("Nothing heard yet.")))
            .children(live.log.iter().enumerate().map(|(i, call)| {
                let entry = &session.plan.entries[call.channel];
                let who = match (&entry.kind, call.talkgroup) {
                    (Kind::OtherTalkgroups { .. }, Some(talkgroup)) => format!("Talkgroup {talkgroup}"),
                    (_, _) => entry.tag.clone(),
                };
                let detail = format!(
                    "{}  ·  {:+.0} dB  ·  {}{}",
                    call.time,
                    call.snr_db,
                    call.secs.map_or("live".to_string(), |secs| format!("{secs:.0} s")),
                    call.unit.map_or(String::new(), |unit| format!("  ·  unit {unit}")),
                );
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(rgb(if call.played { TEXT } else { MUTED }))
                                    .child(who),
                            )
                            .child(div().text_sm().truncate().text_color(rgb(MUTED)).child(detail)),
                    )
                    // Finished calls can be played back from their recording.
                    .when_some(call.recording.clone(), |row, recording| {
                        row.child(button(("replay", i), "Play", false, PRIORITY).on_click(cx.listener(
                            move |this, _, _, cx| {
                                if let Some(s) = &this.session {
                                    s.controls.replay(recording.clone());
                                }
                                cx.notify();
                            },
                        )))
                    })
            }))
    }

    fn systems(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut list = scroller("systems");
        let mut location = None;
        for system in &self.systems {
            if location != Some(&system.location) {
                location = Some(&system.location);
                let title = if system.location.is_empty() {
                    "Systems".to_string()
                } else {
                    system.location.clone()
                };
                list = list.child(div().px_4().pt_3().pb_1().child(heading(title)));
            }
            let (id, selected) = (system.id, self.selected.contains(&system.id));
            let summary = if system.hi_hz > 0.0 {
                format!(
                    "{} channels · {:.1}–{:.1} MHz",
                    system.channels,
                    system.lo_hz / 1e6,
                    system.hi_hz / 1e6
                )
            } else {
                "No channels yet".to_string()
            };
            list = list.child(
                div()
                    .id(("system", id as usize))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_system(id);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_none()
                            .size_5()
                            .rounded_md()
                            .border_2()
                            .border_color(rgb(if selected { ACCENT } else { BORDER }))
                            .when(selected, |mark| mark.bg(rgb(ACCENT))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(rgb(if selected { ACCENT } else { TEXT }))
                                    .child(system.name.clone()),
                            )
                            .child(div().text_sm().truncate().text_color(rgb(MUTED)).child(summary)),
                    ),
            );
        }
        list.child(note(
            "Tap a system to scan it; pick several to scan them together. \
             Systems are added and edited with the desktop app or the command-line tool.",
        ))
    }

    fn settings(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let (volume, squelch_db, gain, scan) = (self.volume, self.squelch_db, self.gain, self.scan);
        let rate = self.rate();
        let muted = self.session.as_ref().is_some_and(|s| s.controls.muted());
        // One setting: its name and the control for it.
        let row = |name: &'static str, control: Div| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(rgb(BORDER))
                .child(div().flex_none().child(name))
                .child(control)
        };
        scroller("settings")
            .child(div().px_4().py_2().child(heading("Settings")))
            .child(row(
                "Volume",
                stepper(
                    format!("{volume:.1}"),
                    button("volume-down", "−", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                        this.set_volume(volume - 0.5);
                        cx.notify();
                    })),
                    button("volume-up", "+", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                        this.set_volume(volume + 0.5);
                        cx.notify();
                    })),
                ),
            ))
            .child(row(
                "Mute",
                div().child(
                    button("mute", if muted { "Muted" } else { "Off" }, muted, ERROR).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Some(s) = &this.session {
                                s.controls.set_muted(!muted);
                            }
                            cx.notify();
                        },
                    )),
                ),
            ))
            .child(row(
                "Squelch",
                stepper(
                    format!("{squelch_db:.0} dB"),
                    button("squelch-down", "−", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                        this.set_squelch(squelch_db - 1.0);
                        cx.notify();
                    })),
                    button("squelch-up", "+", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                        this.set_squelch(squelch_db + 1.0);
                        cx.notify();
                    })),
                ),
            ))
            .child(row(
                "Gain",
                stepper(
                    gain.to_string(),
                    button("gain-down", "−", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                        this.gain = gain.saturating_sub(1);
                        this.save("gain", this.gain);
                        this.start();
                        cx.notify();
                    })),
                    button("gain-up", "+", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                        this.gain = (gain + 1).min(21);
                        this.save("gain", this.gain);
                        this.start();
                        cx.notify();
                    })),
                ),
            ))
            .child(row(
                "Scan mode",
                div()
                    .flex()
                    .gap_1()
                    .children(SCAN_MODES.iter().map(|&(mode, saved_as, title)| {
                        button(saved_as, title, mode == scan, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                            this.scan = mode;
                            this.save("scan_mode", saved_as);
                            this.start();
                            cx.notify();
                        }))
                    })),
            ))
            .child(row(
                "Sample rate",
                div()
                    .flex()
                    .gap_1()
                    .children(self.rates.iter().enumerate().map(|(i, &offered)| {
                        let label = format!("{} MSPS", offered as f64 / 1e6);
                        button(("rate", i), label, Some(offered) == rate, ACCENT).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.sample_rate = offered;
                                this.save("sample_rate", offered);
                                this.start();
                                cx.notify();
                            },
                        ))
                    })),
            ))
            .child(note(match (rate, scan) {
                (None, _) => "Sample rates appear once an Airspy is connected.".to_string(),
                (Some(_), ScanMode::Channels) => {
                    "Scanning one channel at a time always uses the lowest sample rate.".to_string()
                }
                (Some(rate), _) => format!(
                    "A band about {:.1} MHz wide is received at once. The higher rate covers more but is \
                     harder on the battery.",
                    engine::usable_span_hz(rate) / 1e6
                ),
            }))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .child(
                        button("reconnect", "Reconnect Airspy", false, ACCENT).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.connect();
                                cx.notify();
                            },
                        )),
                    ),
            )
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> Div {
        let current = self.tab;
        div()
            .flex()
            .flex_none()
            .bg(rgb(PANEL))
            .border_t_1()
            .border_color(rgb(BORDER))
            .children(TABS.iter().map(|&(tab, title)| {
                div()
                    .id(title)
                    .flex_1()
                    .flex()
                    .justify_center()
                    .py_3()
                    .text_sm()
                    .text_color(rgb(if tab == current { ACCENT } else { MUTED }))
                    .when(tab == current, |item| item.font_weight(FontWeight::BOLD))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
                    .child(title)
            }))
    }
}

impl Render for ScannerApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep clear of the status bar, notch and navigation bar.
        let insets = jni::platform()
            .and_then(|platform| platform.primary_window())
            .map(|window| window.safe_area_insets_logical());
        let (top, bottom) = insets.map_or((0.0, 0.0), |i| (i.top, i.bottom));

        let live = self.session.as_ref().map(|s| s.live.lock().unwrap());
        let body = match (self.tab, &self.session, &live) {
            (Tab::Systems, ..) => self.systems(cx),
            (Tab::Settings, ..) => self.settings(cx),
            (Tab::Channels, Some(session), Some(live)) => self.channels(session, live, cx),
            (Tab::Activity, Some(session), Some(live)) => self.activity(session, live, cx),
            _ => scroller("idle").child(note(
                "Channels appear here once an Airspy is connected and a system is selected.",
            )),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(div().flex_none().h(px(top)).bg(rgb(PANEL)))
            .child(self.now_playing(live.as_deref(), cx))
            .child(body)
            .child(self.tab_bar(cx))
            .child(div().flex_none().h(px(bottom)).bg(rgb(PANEL)))
    }
}

/// The scrolling middle of the screen.
fn scroller(id: &'static str) -> Stateful<Div> {
    div().id(id).flex_1().overflow_y_scroll()
}

fn heading(title: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(MUTED))
        .child(title.into())
}

fn note(text: impl Into<SharedString>) -> Div {
    div().px_4().py_3().text_sm().text_color(rgb(MUTED)).child(text.into())
}

/// A button big enough for a finger; `active` fills it with `color`.
fn button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, active: bool, color: u32) -> Stateful<Div> {
    let (bg, fg) = if active { (color, BG) } else { (RAISED, TEXT) };
    div()
        .id(id)
        .flex_none()
        .px_3()
        .py_2()
        .rounded_md()
        .text_sm()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .child(label.into())
}

fn stepper(value: String, down: Stateful<Div>, up: Stateful<Div>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(down)
        .child(div().w(px(64.)).flex().justify_center().child(value))
        .child(up)
}
