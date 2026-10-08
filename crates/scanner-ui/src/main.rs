//! Desktop front end for the scanner.

// On Windows, don't open a console window alongside the app.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod session;

use std::time::Duration;

use std::path::Path;

use gpui::{
    App, Application, Bounds, Context, CursorStyle, Decorations, Div, FontWeight, MouseButton, PathPromptOptions,
    Pixels, ResizeEdge, Rgba, SharedString, Stateful, TitlebarOptions, Window, WindowBounds, WindowDecorations,
    WindowOptions, div, prelude::*, px, relative, rgb, size,
};
use scanner::db::{Db, System};
use scanner::engine::{self, Config};
use scanner::plan::{Kind, Plan};
use session::{Live, Session};

const BG: u32 = 0x14171c;
const PANEL: u32 = 0x1c2027;
const RAISED: u32 = 0x2a303a;
const BORDER: u32 = 0x313743;
const TEXT: u32 = 0xe6e9ee;
const MUTED: u32 = 0x8b93a1;
const ACCENT: u32 = 0x4cc38a;
const HOLD: u32 = 0xe0a84c;
const ERROR: u32 = 0xe5645c;

/// Signal level that fills a channel's meter, in dB over the noise floor.
const METER_FULL_DB: f32 = 40.0;
const REFRESH: Duration = Duration::from_millis(100);

const TITLE: &str = "Airspy Scanner";
const DEFAULT_SIZE: (f32, f32) = (1280., 760.);
const MIN_SIZE: (f32, f32) = (640., 320.);
const SIDEBAR_WIDTH: f32 = 220.;
/// Share of the display a new window may take up.
const MAX_DISPLAY_SHARE: f32 = 0.85;
/// Below these window widths the activity log, then the channel
/// descriptions and the title, make way for the rest.
const ACTIVITY_MIN_WIDTH: f32 = 1100.;
const DETAIL_MIN_WIDTH: f32 = 900.;
/// Width of the strips along the window edges that resize it.
const RESIZE_BORDER: f32 = 5.;
const RESIZE_CORNER: f32 = 14.;

struct ScannerView {
    /// `None` if the channel database couldn't be opened.
    db: Option<Db>,
    systems: Vec<System>,
    /// Systems being scanned together, in the order they were picked.
    selected: Vec<i64>,
    /// The Airspy's sample rate, if one was found at startup.
    rate: Option<u32>,
    /// `None` while nothing is selected or the scan couldn't start.
    session: Option<Session>,
    /// What went wrong last, shown until the next action succeeds.
    error: Option<String>,
    volume: f32,
    squelch_db: f32,
}

impl ScannerView {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            db: None,
            systems: Vec::new(),
            selected: Vec::new(),
            rate: None,
            session: None,
            error: None,
            volume: 3.0,
            squelch_db: 6.0,
        };
        match Db::open(&Db::default_path()) {
            Ok(db) => view.db = Some(db),
            Err(e) => view.error = Some(e),
        }
        // Asked once, up front: the Airspy can't be queried while in use.
        match engine::sample_rate(&Config::default()) {
            Ok(rate) => view.rate = Some(rate),
            Err(e) => view.error = Some(e),
        }
        view.reload_systems();
        if let (Some(first), None) = (view.systems.first(), &view.error) {
            view.selected = vec![first.id];
            view.start();
        }

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        // The engine owns the Airspy and the audio player; shut it down
        // before the process goes away.
        cx.on_app_quit(|this, _| {
            this.session = None;
            async {}
        })
        .detach();
        view
    }

    fn reload_systems(&mut self) {
        if let Some(db) = &self.db {
            match db.systems() {
                Ok(systems) => self.systems = systems,
                Err(e) => self.error = Some(e),
            }
        }
        self.selected.retain(|id| self.systems.iter().any(|s| s.id == *id));
    }

    fn plan(&self, systems: &[i64]) -> Result<Plan, String> {
        self.db.as_ref().ok_or("no channel database")?.plan(systems)
    }

    /// Scan the selected systems.
    fn start(&mut self) {
        // Only one receiver can hold the Airspy: stop the old one first.
        self.session = None;
        self.error = None;
        if self.selected.is_empty() {
            return;
        }
        match self.plan(&self.selected) {
            Ok(plan) => {
                let config = Config {
                    rate: self.rate,
                    ..Config::default()
                };
                self.session = Some(Session::start(plan, config, self.volume, self.squelch_db));
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Add a system to the scan or take it out. Systems too far apart in
    /// frequency to share one tuning replace the selection instead.
    fn toggle_system(&mut self, id: i64) {
        if let Some(i) = self.selected.iter().position(|s| *s == id) {
            self.selected.remove(i);
        } else {
            let mut both = self.selected.clone();
            both.push(id);
            let fits = self.plan(&both).is_ok_and(|plan| {
                let (lo, hi, _) = plan.span();
                self.rate.is_some_and(|rate| hi - lo <= engine::usable_span_hz(rate))
            });
            self.selected = if fits { both } else { vec![id] };
        }
        self.start();
    }

    fn import(&mut self, path: &Path) {
        let Some(db) = &mut self.db else { return };
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let added = Plan::from_file(path, None).and_then(|(plan, _)| db.add_system(&name, "Imported", &plan));
        match added {
            Ok(_) => self.error = None,
            Err(e) => self.error = Some(e),
        }
        self.reload_systems();
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.5, 10.0);
        if let Some(s) = &self.session {
            s.controls.set_volume(self.volume);
        }
    }

    fn set_squelch(&mut self, db: f32) {
        self.squelch_db = db.clamp(3.0, 30.0);
        if let Some(s) = &self.session {
            s.controls.set_squelch_db(self.squelch_db);
        }
    }

    fn header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Wayland compositors like GNOME's draw no title bar for us, so this
        // bar is one: drag to move, double-click to maximize.
        let own_frame = matches!(window.window_decorations(), Decorations::Client { .. });
        let roomy = window.bounds().size.width >= px(DETAIL_MIN_WIDTH);
        let muted = self.session.as_ref().is_some_and(|s| s.controls.muted());
        let (volume, squelch_db) = (self.volume, self.squelch_db);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            .when(own_frame, |bar| {
                bar.on_mouse_down(MouseButton::Left, |event, window, _| {
                    if event.click_count == 2 {
                        window.zoom_window();
                    } else {
                        window.start_window_move();
                    }
                })
                .on_mouse_down(MouseButton::Right, |event, window, _| {
                    window.show_window_menu(event.position)
                })
            })
            .when(roomy, |bar| {
                bar.child(div().flex_none().font_weight(FontWeight::BOLD).child(TITLE))
            })
            .child(div().flex_1())
            .child(stepper(
                "Squelch",
                format!("{squelch_db:.0} dB"),
                button("squelch-down", "−", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_squelch(squelch_db - 1.0);
                    cx.notify();
                })),
                button("squelch-up", "+", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_squelch(squelch_db + 1.0);
                    cx.notify();
                })),
            ))
            .child(stepper(
                "Volume",
                format!("{volume:.1}"),
                button("volume-down", "−", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_volume(volume - 0.5);
                    cx.notify();
                })),
                button("volume-up", "+", false, ACCENT).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_volume(volume + 0.5);
                    cx.notify();
                })),
            ))
            .child(
                button("mute", if muted { "Muted" } else { "Mute" }, muted, ERROR).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(s) = &this.session {
                            s.controls.set_muted(!muted);
                        }
                        cx.notify();
                    },
                )),
            )
            .when(own_frame, |bar| {
                bar.child(
                    div()
                        .flex()
                        .flex_none()
                        .gap_1()
                        .pl_2()
                        .child(button("minimize", "–", false, ACCENT).on_click(|_, window, _| window.minimize_window()))
                        .child(button("maximize", "□", false, ACCENT).on_click(|_, window, _| window.zoom_window()))
                        .child(button("close", "✕", false, ERROR).on_click(|_, window, _| window.remove_window())),
                )
            })
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div()
            .id("systems")
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL));
        let mut location = None;
        for system in &self.systems {
            if location != Some(&system.location) {
                location = Some(&system.location);
                let title = if system.location.is_empty() {
                    "Systems".to_string()
                } else {
                    system.location.clone()
                };
                list = list.child(section_title(title));
            }
            let (id, selected) = (system.id, self.selected.contains(&system.id));
            list = list.child(
                div()
                    .id(("system", id as usize))
                    .mx_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |item| item.bg(rgb(RAISED)))
                    .hover(|style| style.bg(rgb(RAISED)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_system(id);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .text_color(rgb(if selected { ACCENT } else { TEXT }))
                            .child(system.name.clone()),
                    )
                    .child(div().text_xs().truncate().text_color(rgb(MUTED)).child(format!(
                        "{} channels · {:.1}–{:.1} MHz",
                        system.channels,
                        system.lo_hz / 1e6,
                        system.hi_hz / 1e6
                    ))),
            );
        }
        list.child(
            div()
                .px_4()
                .py_3()
                .child(
                    button("import", "Import…", false, ACCENT).on_click(cx.listener(|_, _, _, cx| {
                        let picked = cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some("Import channel file".into()),
                        });
                        cx.spawn(async move |this, cx| {
                            if let Ok(Ok(Some(paths))) = picked.await {
                                this.update(cx, |this, cx| {
                                    paths.iter().for_each(|path| this.import(path));
                                    cx.notify();
                                })
                                .ok();
                            }
                        })
                        .detach();
                    })),
                ),
        )
    }

    fn now_playing(&self, session: &Session, live: &Live) -> impl IntoElement {
        let pinned = session.controls.pinned();
        let (title, detail, color) = match (&live.error, live.playing, pinned) {
            (Some(error), ..) => ("Receiver stopped".to_string(), error.clone(), ERROR),
            (None, Some(c), _) => {
                let e = &session.plan.entries[c];
                (e.tag.clone(), format!("{}  ·  {}", e.desc, e.id()), ACCENT)
            }
            (None, None, Some(c)) => {
                let e = &session.plan.entries[c];
                (
                    e.tag.clone(),
                    format!("Holding on {}, waiting for traffic", e.id()),
                    HOLD,
                )
            }
            (None, None, None) => {
                let (lo, hi, _) = session.plan.span();
                (
                    "Scanning".to_string(),
                    format!(
                        "{} channels, {:.1}–{:.1} MHz",
                        session.plan.entries.len(),
                        lo / 1e6,
                        hi / 1e6
                    ),
                    MUTED,
                )
            }
        };
        div()
            .flex_none()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(color))
                    .child(title),
            )
            .child(div().text_sm().text_color(rgb(MUTED)).child(detail))
    }

    fn channel_row(&self, c: usize, roomy: bool, session: &Session, live: &Live, cx: &mut Context<Self>) -> Div {
        let e = &session.plan.entries[c];
        let skipped = session.controls.skipped(c);
        let pinned = session.controls.pinned() == Some(c);
        let (open, playing) = (live.open[c], live.playing == Some(c));
        let level = (live.levels[c] / METER_FULL_DB).clamp(0.0, 1.0);
        let color = if playing {
            ACCENT
        } else if open {
            TEXT
        } else {
            MUTED
        };

        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_1()
            .text_sm()
            .when(playing, |row| row.bg(rgb(RAISED)))
            .when(skipped, |row| row.opacity(0.4))
            .child(
                div()
                    .flex_none()
                    .size_2()
                    .rounded_full()
                    .bg(rgb(if open { ACCENT } else { BORDER })),
            )
            .child(div().flex_none().w(px(70.)).text_color(rgb(MUTED)).child(e.id()))
            .child(
                div()
                    .when(roomy, |tag| tag.flex_none().w(px(130.)))
                    .when(!roomy, |tag| tag.flex_1())
                    .truncate()
                    .text_color(rgb(color))
                    .child(e.tag.clone()),
            )
            .when(roomy, |row| {
                row.child(div().flex_1().truncate().text_color(rgb(MUTED)).child(e.desc.clone()))
            })
            .child(
                div().flex_none().w(px(70.)).h_1().rounded_full().bg(rgb(RAISED)).child(
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
                    .text_color(rgb(MUTED))
                    .child(match live.calls[c] {
                        0 => String::new(),
                        n => format!("×{n}"),
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(62.))
                    .text_color(rgb(MUTED))
                    .child(live.last_heard[c].clone().unwrap_or_default()),
            )
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
                        // Skips are remembered with the channel.
                        if let (Some(db), Some(channel)) = (&this.db, s.plan.entries[c].channel_id) {
                            db.set_skip(channel, !skipped).ok();
                        }
                    }
                    cx.notify();
                })),
            )
    }

    fn activity(&self, session: &Session, live: &Live) -> impl IntoElement {
        div()
            .id("activity")
            .flex_none()
            .w(px(330.))
            .h_full()
            .overflow_y_scroll()
            .border_l_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL))
            .child(section_title("Activity"))
            .when(live.log.is_empty(), |log| {
                log.child(
                    div()
                        .px_4()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child("Nothing heard yet."),
                )
            })
            .children(live.log.iter().map(|call| {
                div()
                    .flex()
                    .gap_3()
                    .px_4()
                    .py_1()
                    .text_sm()
                    .child(div().flex_none().text_color(rgb(MUTED)).child(call.time.clone()))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(rgb(if call.played { TEXT } else { MUTED }))
                            .child(match (&session.plan.entries[call.channel].kind, call.talkgroup) {
                                (Kind::OtherTalkgroups { .. }, Some(talkgroup)) => format!("Talkgroup {talkgroup}"),
                                (_, _) => session.plan.entries[call.channel].tag.clone(),
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(MUTED))
                            .child(format!("{:+.0} dB", call.snr_db)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(40.))
                            .text_color(rgb(MUTED))
                            .child(match call.secs {
                                Some(secs) => format!("{secs:.0}s"),
                                None => "live".into(),
                            }),
                    )
            }))
    }
}

impl Render for ScannerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.bounds().size.width;
        let roomy = width >= px(DETAIL_MIN_WIDTH);
        let main = div().flex_1().h_full().flex().flex_col().overflow_hidden();
        let main = match &self.session {
            None => {
                let message = match (&self.error, self.systems.is_empty()) {
                    (Some(error), _) => div().text_color(rgb(ERROR)).child(error.clone()),
                    (None, true) => div()
                        .text_color(rgb(MUTED))
                        .child("The channel database is empty. Import a channel file to begin."),
                    (None, false) => div()
                        .text_color(rgb(MUTED))
                        .child("Pick a system on the left to scan it."),
                };
                main.child(message.p_4())
            }
            Some(session) => {
                let live = session.live.lock().unwrap();
                main.child(self.now_playing(session, &live))
                    .when_some(self.error.clone(), |main, error| {
                        main.child(
                            div()
                                .flex_none()
                                .px_4()
                                .py_2()
                                .text_sm()
                                .text_color(rgb(ERROR))
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            // Lets the lists scroll instead of stretching the window.
                            .overflow_hidden()
                            .child(
                                div()
                                    .id("channels")
                                    .flex_1()
                                    .h_full()
                                    .overflow_y_scroll()
                                    .child(section_title("Channels"))
                                    .children(
                                        (0..session.plan.entries.len())
                                            .map(|c| self.channel_row(c, roomy, session, &live, cx)),
                                    ),
                            )
                            .when(width >= px(ACTIVITY_MIN_WIDTH), |body| {
                                body.child(self.activity(session, &live))
                            }),
                    )
            }
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(self.header(window, cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    .child(self.sidebar(cx))
                    .child(main),
            )
            .children(resize_handles(window))
    }
}

/// Strips along the edges and corners that resize the window, for when the
/// compositor leaves the window frame to us.
fn resize_handles(window: &Window) -> Vec<Div> {
    if !matches!(window.window_decorations(), Decorations::Client { .. })
        || window.is_maximized()
        || window.is_fullscreen()
    {
        return Vec::new();
    }
    let (border, corner) = (px(RESIZE_BORDER), px(RESIZE_CORNER));
    let handle = |edge: ResizeEdge, cursor: CursorStyle, place: fn(Div, Pixels, Pixels) -> Div| {
        place(div().absolute(), border, corner)
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            })
    };
    use CursorStyle::{ResizeLeftRight, ResizeUpDown, ResizeUpLeftDownRight, ResizeUpRightDownLeft};
    vec![
        handle(ResizeEdge::Top, ResizeUpDown, |d, b, _| {
            d.top_0().left_0().right_0().h(b)
        }),
        handle(ResizeEdge::Bottom, ResizeUpDown, |d, b, _| {
            d.bottom_0().left_0().right_0().h(b)
        }),
        handle(ResizeEdge::Left, ResizeLeftRight, |d, b, _| {
            d.left_0().top_0().bottom_0().w(b)
        }),
        handle(ResizeEdge::Right, ResizeLeftRight, |d, b, _| {
            d.right_0().top_0().bottom_0().w(b)
        }),
        // Corners go last so they sit on top of the edges.
        handle(ResizeEdge::TopLeft, ResizeUpLeftDownRight, |d, _, c| {
            d.top_0().left_0().size(c)
        }),
        handle(ResizeEdge::TopRight, ResizeUpRightDownLeft, |d, _, c| {
            d.top_0().right_0().size(c)
        }),
        handle(ResizeEdge::BottomLeft, ResizeUpRightDownLeft, |d, _, c| {
            d.bottom_0().left_0().size(c)
        }),
        handle(ResizeEdge::BottomRight, ResizeUpLeftDownRight, |d, _, c| {
            d.bottom_0().right_0().size(c)
        }),
    ]
}

fn section_title(title: impl Into<SharedString>) -> Div {
    div()
        .px_4()
        .pt_3()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(MUTED))
        .child(title.into())
}

/// A small push button; `active` fills it with `color`.
fn button(id: impl Into<gpui::ElementId>, label: &'static str, active: bool, color: u32) -> Stateful<Div> {
    let (bg, fg): (Rgba, Rgba) = if active {
        (rgb(color), rgb(BG))
    } else {
        (rgb(RAISED), rgb(TEXT))
    };
    div()
        .id(id)
        .flex_none()
        .px_2()
        .rounded_md()
        .text_sm()
        .bg(bg)
        .text_color(fg)
        .cursor_pointer()
        .hover(|style| style.opacity(0.8))
        // Keep clicks from also dragging the window by its header.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label)
}

fn stepper(label: &'static str, value: String, down: Stateful<Div>, up: Stateful<Div>) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_sm()
        .child(div().text_color(rgb(MUTED)).child(label))
        .child(down)
        .child(
            div()
                .w(px(46.))
                .flex()
                .justify_center()
                .child(SharedString::from(value)),
        )
        .child(up)
}

fn main() {
    Application::new().run(|cx: &mut App| {
        // Never open larger than the screen can show.
        let mut window_size = size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1));
        if let Some(display) = cx.primary_display().or_else(|| cx.displays().into_iter().next()) {
            let screen = display.bounds().size;
            window_size.width = window_size.width.min(screen.width * MAX_DISPLAY_SHARE);
            window_size.height = window_size.height.min(screen.height * MAX_DISPLAY_SHARE);
        }
        let bounds = Bounds::centered(None, window_size, cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(TITLE.into()),
                    ..Default::default()
                }),
                // Asking for server-side decorations gets none at all on
                // GNOME; draw our own wherever the compositor leaves it to us.
                window_decorations: Some(WindowDecorations::Client),
                window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
                app_id: Some("airspy-scanner".into()),
                ..Default::default()
            },
            |_, cx| cx.new(ScannerView::new),
        )
        .unwrap();
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
