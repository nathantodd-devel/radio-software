//! The page's one view: what is playing, the channels, the activity log, the
//! systems to pick from and the settings. Everything shown comes from the
//! server, and every button only asks the server to change something.

use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{Context, Div, FontWeight, SharedString, Stateful, Window, div, prelude::*, px, relative, rgb};
use scanner_proto::{Channel, ChannelState, ClientMessage, Options, ServerMessage, Status, System};

use crate::audio::Audio;
use crate::net::{Connection, Incoming};

// Matches the page's background in index.html, so loading hands over to the
// first frame without a flash.
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
/// How long to wait before trying again after losing the server.
const RETRY: Duration = Duration::from_secs(2);
/// From this width up, the systems get a column of their own.
const WIDE: f32 = 900.0;

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

pub struct Client {
    /// `None` while the server can't be reached.
    connection: Option<Connection>,
    /// Where a connection puts what it receives.
    incoming: UnboundedSender<Incoming>,
    audio: Rc<Audio>,
    systems: Vec<System>,
    channels: Vec<Channel>,
    /// `None` until the server has said.
    options: Option<Options>,
    status: Status,
    tab: Tab,
    /// The channel whose buttons are showing.
    expanded: Option<usize>,
    /// Show only channels with a transmission on them.
    active_only: bool,
    /// The page is wide enough for the systems to have a column of their own.
    wide: bool,
}

impl Client {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (incoming, mut received) = unbounded();
        let mut client = Self {
            connection: None,
            incoming,
            audio: Rc::new(Audio::default()),
            systems: Vec::new(),
            channels: Vec::new(),
            options: None,
            status: Status::default(),
            tab: Tab::Channels,
            expanded: None,
            active_only: false,
            wide: false,
        };
        client.connect();

        cx.spawn(async move |this, cx| {
            while let Some(incoming) = received.next().await {
                let lost = matches!(incoming, Incoming::Closed);
                if this.update(cx, |this, cx| this.receive(incoming, cx)).is_err() {
                    break;
                }
                if lost {
                    cx.background_executor().timer(RETRY).await;
                    this.update(cx, |this, _| this.connect()).ok();
                }
            }
        })
        .detach();
        client
    }

    fn connect(&mut self) {
        self.connection = Connection::open(self.incoming.clone(), self.audio.clone());
        if self.connection.is_none() {
            // Count it as lost, so it is tried again.
            self.incoming.unbounded_send(Incoming::Closed).ok();
        }
    }

    fn receive(&mut self, incoming: Incoming, cx: &mut Context<Self>) {
        match incoming {
            Incoming::Closed => self.connection = None,
            Incoming::Message(message) => match *message {
                ServerMessage::Systems { systems } => self.systems = systems,
                ServerMessage::Channels { channels } => {
                    self.channels = channels;
                    self.expanded = None;
                }
                ServerMessage::Options(options) => self.options = Some(options),
                ServerMessage::Status(status) => self.status = status,
            },
        }
        cx.notify();
    }

    fn send(&self, message: ClientMessage) {
        if let Some(connection) = &self.connection {
            connection.send(&message);
        }
    }

    /// A button that asks the server for something when clicked.
    fn asks(
        &self,
        button: Stateful<Div>,
        message: impl Fn() -> ClientMessage + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        button.on_click(cx.listener(move |this, _, _, _| this.send(message())))
    }

    // ── What is playing ─────────────────────────────────────────────────

    fn now_playing(&self, cx: &mut Context<Self>) -> Div {
        let status = &self.status;
        let channel = |c: usize| self.channels.get(c);
        let (title, detail, color) = if self.connection.is_none() {
            (
                "Not connected".to_string(),
                "Trying to reach the scanner…".to_string(),
                ERROR,
            )
        } else if let Some(problem) = &status.problem {
            ("Not scanning".into(), problem.clone(), ERROR)
        } else if status.replaying {
            ("Replaying".into(), "A recorded call".into(), PRIORITY)
        } else if let Some(playing) = status.playing.and_then(channel) {
            (
                playing.name.clone(),
                format!("{} · {}", playing.description, playing.label),
                ACCENT,
            )
        } else if let Some(held) = status.held.and_then(channel) {
            (held.name.clone(), format!("Holding on {}", held.label), HOLD)
        } else {
            let detail = match status.band {
                Some(band) if band.lo_hz == band.hi_hz => {
                    format!(
                        "Frequency {} of {} · {:.4} MHz",
                        band.index + 1,
                        band.count,
                        band.lo_hz / 1e6
                    )
                }
                Some(band) => format!(
                    "Band {} of {} · {:.1}–{:.1} MHz",
                    band.index + 1,
                    band.count,
                    band.lo_hz / 1e6,
                    band.hi_hz / 1e6
                ),
                None => format!("{} channels", self.channels.len()),
            };
            ("Scanning".into(), detail, MUTED)
        };
        let listening = self.audio.listening();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .child(div().text_xs().text_color(rgb(MUTED)).child("AIRSPY SCANNER"))
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(color))
                            .truncate()
                            .child(title),
                    )
                    .child(div().text_sm().truncate().text_color(rgb(MUTED)).child(detail)),
            )
            // Sound is off until asked for: browsers won't play any before
            // the page has been clicked.
            .child(
                button(
                    "listen",
                    if listening { "Listening" } else { "Listen" },
                    listening,
                    ACCENT,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if listening {
                        this.audio.stop();
                    } else {
                        this.audio.start();
                    }
                    cx.notify();
                })),
            )
    }

    // ── Channels ────────────────────────────────────────────────────────

    fn channels(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        if self.channels.is_empty() {
            return scroller("channels").child(note("Channels appear here once a system is being scanned."));
        }
        let active_only = self.active_only;
        let state = |c: usize| self.status.channels.get(c).cloned().unwrap_or_default();
        let shown: Vec<usize> = (0..self.channels.len())
            .filter(|&c| !active_only || state(c).open)
            .collect();
        scroller("channels")
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .child(heading(format!("{} channels", self.channels.len())))
                    .child(
                        button("active-only", "Active only", active_only, ACCENT).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.active_only = !active_only;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .when(shown.is_empty(), |list| list.child(note("Nothing is transmitting.")))
            .children(shown.into_iter().map(|c| self.channel_row(c, state(c), cx)))
    }

    fn channel_row(&self, c: usize, state: ChannelState, cx: &mut Context<Self>) -> Div {
        let channel = &self.channels[c];
        let (playing, held) = (self.status.playing == Some(c), self.status.held == Some(c));
        let ChannelState {
            open,
            skipped,
            priority,
            recorded,
            ..
        } = state;
        let expanded = self.expanded == Some(c);
        let level = (state.level_db / METER_FULL_DB).clamp(0.0, 1.0);
        let color = if playing {
            ACCENT
        } else if open {
            TEXT
        } else {
            MUTED
        };
        // Marks that apply to the channel, shown beside its name.
        let marks: String = [
            (held, " ·hold"),
            (priority, " ·pri"),
            (recorded, " ·rec"),
            (skipped, " ·skip"),
        ]
        .iter()
        .filter(|m| m.0)
        .map(|m| m.1)
        .collect();

        let summary = div()
            .id(("channel", c))
            .cursor_pointer()
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
                            .child(format!("{}{marks}", channel.name)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .text_color(rgb(MUTED))
                            .child(format!("{} · {}", channel.label, channel.description)),
                    ),
            )
            // There is only room for when it was last heard on a wide page.
            .when(self.wide, |row| {
                row.child(
                    div()
                        .flex_none()
                        .w(px(72.))
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(state.last_heard.clone().unwrap_or_default()),
                )
            })
            .child(
                div().flex_none().w(px(64.)).h_1().rounded_full().bg(rgb(RAISED)).child(
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
                    .w(px(40.))
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(match state.calls {
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
        row.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .px_4()
                .pb_3()
                .child(self.asks(
                    button(("hold", c), "Hold", held, HOLD),
                    move || ClientMessage::Hold {
                        channel: (!held).then_some(c),
                    },
                    cx,
                ))
                .child(self.asks(
                    button(("skip", c), "Skip", skipped, ERROR),
                    move || ClientMessage::Skip {
                        channel: c,
                        on: !skipped,
                    },
                    cx,
                ))
                .child(self.asks(
                    button(("priority", c), "Priority", priority, PRIORITY),
                    move || ClientMessage::Priority {
                        channel: c,
                        on: !priority,
                    },
                    cx,
                ))
                .child(self.asks(
                    button(("record", c), "Record", recorded, ERROR),
                    move || ClientMessage::Record {
                        channel: c,
                        on: !recorded,
                    },
                    cx,
                )),
        )
    }

    // ── Activity ────────────────────────────────────────────────────────

    fn activity(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        scroller("activity")
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .child(heading("Activity"))
                    .when(self.status.replaying, |title| {
                        title.child(self.asks(
                            button("stop-replay", "Stop replay", true, PRIORITY),
                            || ClientMessage::StopReplay,
                            cx,
                        ))
                    }),
            )
            .when(self.status.log.is_empty(), |log| log.child(note("Nothing heard yet.")))
            .children(self.status.log.iter().map(|call| {
                let detail = format!(
                    "{}  ·  {:+.0} dB  ·  {}{}",
                    call.time,
                    call.snr_db,
                    call.secs.map_or("live".to_string(), |secs| format!("{secs:.0} s")),
                    call.unit.map_or(String::new(), |unit| format!("  ·  unit {unit}")),
                );
                let id = call.id;
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
                                    .child(call.name.clone()),
                            )
                            .child(div().text_sm().truncate().text_color(rgb(MUTED)).child(detail)),
                    )
                    // Finished calls can be played back from their recording.
                    .when(call.recorded, |row| {
                        row.child(self.asks(
                            button(("replay", id as usize), "Play", false, PRIORITY),
                            move || ClientMessage::Replay { call: id },
                            cx,
                        ))
                    })
            }))
    }

    // ── Systems ─────────────────────────────────────────────────────────

    fn systems(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        scroller("systems")
            .child(div().px_4().py_2().child(heading("Systems")))
            .when(self.systems.is_empty(), |list| {
                list.child(note(
                    "The channel database is empty. Add systems with the desktop app or the scanner command.",
                ))
            })
            .children(self.systems.iter().map(|system| {
                let (id, selected) = (system.id, system.selected);
                let span = if system.channels == 0 {
                    "no channels".to_string()
                } else {
                    format!(
                        "{} channels · {:.1}–{:.1} MHz",
                        system.channels,
                        system.lo_hz / 1e6,
                        system.hi_hz / 1e6
                    )
                };
                let detail = match system.location.as_str() {
                    "" => span,
                    location => format!("{location} · {span}"),
                };
                div()
                    .id(("system", id as usize))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .when(selected, |row| row.bg(rgb(PANEL)))
                    .on_click(cx.listener(move |this, _, _, _| this.send(ClientMessage::ToggleSystem { id })))
                    .child(
                        div()
                            .flex_none()
                            .size_3()
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(if selected { ACCENT } else { BORDER }))
                            .when(selected, |mark| mark.bg(rgb(ACCENT))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .child(div().truncate().child(system.name.clone()))
                            .child(div().text_sm().truncate().text_color(rgb(MUTED)).child(detail)),
                    )
            }))
    }

    // ── Settings ────────────────────────────────────────────────────────

    fn settings(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let Some(options) = &self.options else {
            return scroller("settings").child(note("Waiting for the scanner…"));
        };
        // One setting: its name and the control for it.
        let row = |name: &'static str, control: Div| {
            div()
                .flex()
                .flex_wrap()
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
        let choices = || div().flex().flex_wrap().justify_end().gap_2();
        let (volume, squelch_db, gain, ppm) = (options.volume, options.squelch_db, options.gain, options.ppm);
        let (muted, bias_tee) = (options.muted, options.bias_tee);

        let receiver = match (&options.receiver, options.span_hz) {
            (Some(name), Some(span)) => format!("{name} · receives {:.1} MHz at once", span / 1e6),
            (Some(name), None) => name.clone(),
            (None, _) => "No receiver found.".to_string(),
        };
        scroller("settings")
            .child(div().px_4().py_2().child(heading("Settings")))
            .child(note(
                "These are the scanner's settings: changing one changes it for everyone listening.",
            ))
            .child(row(
                "Volume",
                stepper(
                    format!("{volume:.1}"),
                    self.asks(
                        button("volume-down", "−", false, ACCENT),
                        move || ClientMessage::SetVolume { volume: volume - 0.5 },
                        cx,
                    ),
                    self.asks(
                        button("volume-up", "+", false, ACCENT),
                        move || ClientMessage::SetVolume { volume: volume + 0.5 },
                        cx,
                    ),
                ),
            ))
            .child(row(
                "Mute",
                div().child(self.asks(
                    button("mute", if muted { "Muted" } else { "Off" }, muted, ERROR),
                    move || ClientMessage::SetMuted { muted: !muted },
                    cx,
                )),
            ))
            .child(row(
                "Squelch",
                stepper(
                    format!("{squelch_db:.0} dB"),
                    self.asks(
                        button("squelch-down", "−", false, ACCENT),
                        move || ClientMessage::SetSquelch { db: squelch_db - 1.0 },
                        cx,
                    ),
                    self.asks(
                        button("squelch-up", "+", false, ACCENT),
                        move || ClientMessage::SetSquelch { db: squelch_db + 1.0 },
                        cx,
                    ),
                ),
            ))
            .child(row(
                "Scan mode",
                choices().children(options.scan_modes.iter().enumerate().map(|(i, mode)| {
                    let id = mode.id.clone();
                    self.asks(
                        button(
                            ("scan-mode", i),
                            mode.name.clone(),
                            mode.id == options.scan_mode,
                            ACCENT,
                        ),
                        move || ClientMessage::SetScanMode { id: id.clone() },
                        cx,
                    )
                })),
            ))
            .child(row(
                "Receiver",
                choices().children(options.devices.iter().enumerate().map(|(i, device)| {
                    let id = device.id.clone();
                    self.asks(
                        button(("device", i), device.name.clone(), device.id == options.device, ACCENT),
                        move || ClientMessage::SetDevice { id: id.clone() },
                        cx,
                    )
                })),
            ))
            .child(note(receiver))
            .when(!options.sample_rates.is_empty(), |page| {
                page.child(row(
                    "Sample rate",
                    choices().children(options.sample_rates.iter().enumerate().map(|(i, &rate)| {
                        self.asks(
                            button(("rate", i), rate_label(rate), Some(rate) == options.sample_rate, ACCENT),
                            move || ClientMessage::SetSampleRate { rate },
                            cx,
                        )
                    })),
                ))
            })
            .child(row(
                "Gain",
                stepper(
                    format!("{gain}"),
                    self.asks(
                        button("gain-down", "−", false, ACCENT),
                        move || ClientMessage::SetGain {
                            gain: gain.saturating_sub(1),
                        },
                        cx,
                    ),
                    self.asks(
                        button("gain-up", "+", false, ACCENT),
                        move || ClientMessage::SetGain { gain: gain + 1 },
                        cx,
                    ),
                ),
            ))
            .child(row(
                "Frequency correction",
                stepper(
                    format!("{ppm} ppm"),
                    self.asks(
                        button("ppm-down", "−", false, ACCENT),
                        move || ClientMessage::SetPpm { ppm: ppm - 1 },
                        cx,
                    ),
                    self.asks(
                        button("ppm-up", "+", false, ACCENT),
                        move || ClientMessage::SetPpm { ppm: ppm + 1 },
                        cx,
                    ),
                ),
            ))
            .child(row(
                "Antenna power (bias tee)",
                div().child(self.asks(
                    button("bias-tee", if bias_tee { "On" } else { "Off" }, bias_tee, HOLD),
                    move || ClientMessage::SetBiasTee { on: !bias_tee },
                    cx,
                )),
            ))
    }

    fn tab_bar(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let current = self.tab(wide);
        div()
            .flex()
            .flex_none()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            // With a column of their own, the systems need no tab.
            .children(
                TABS.iter()
                    .filter(|(tab, _)| !(wide && *tab == Tab::Systems))
                    .map(|&(tab, title)| {
                        div()
                            .id(title)
                            .cursor_pointer()
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
                    }),
            )
    }

    /// The tab showing: on a wide page the systems are always in view, so
    /// their tab stands for the channels.
    fn tab(&self, wide: bool) -> Tab {
        match self.tab {
            Tab::Systems if wide => Tab::Channels,
            tab => tab,
        }
    }
}

impl Render for Client {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wide = window.viewport_size().width >= px(WIDE);
        self.wide = wide;
        let body = match self.tab(wide) {
            Tab::Channels => self.channels(cx),
            Tab::Activity => self.activity(cx),
            Tab::Systems => self.systems(cx),
            Tab::Settings => self.settings(cx),
        };
        let main = div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.tab_bar(wide, cx))
            .child(body);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(self.now_playing(cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    .when(wide, |page| {
                        page.child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_col()
                                .w(px(300.))
                                .border_r_1()
                                .border_color(rgb(BORDER))
                                .child(self.systems(cx)),
                        )
                    })
                    .child(main),
            )
    }
}

/// A sample rate as it is usually written: "10 MSPS", "2.048 MSPS".
fn rate_label(rate: u32) -> String {
    let msps = format!("{:.3}", f64::from(rate) / 1e6);
    format!("{} MSPS", msps.trim_end_matches('0').trim_end_matches('.'))
}

/// The scrolling part of a column.
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
        .cursor_pointer()
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
        .child(div().w(px(72.)).flex().justify_center().child(value))
        .child(up)
}
