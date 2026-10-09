//! Desktop front end for the scanner.

// On Windows, don't open a console window alongside the app.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod editor;
mod session;
mod theme;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use gpui::{
    App, Application, Bounds, Context, CursorStyle, Decorations, Div, FocusHandle, FontWeight, KeyDownEvent,
    MouseButton, PathPromptOptions, Pixels, ResizeEdge, Rgba, SharedString, Stateful, TitlebarOptions, Window,
    WindowBounds, WindowDecorations, WindowOptions, div, prelude::*, px, relative, rgb, size,
};
use scanner::db::{Db, System};
use scanner::engine::{self, Config};
use scanner::plan::{Kind, Plan};
use session::{Live, Session};
use theme::theme;

/// Signal level that fills a channel's meter, in dB over the noise floor.
const METER_FULL_DB: f32 = 40.0;
const REFRESH: Duration = Duration::from_millis(100);

const TITLE: &str = "Airspy Scanner";
const DEFAULT_SIZE: (f32, f32) = (1460., 760.);
const MIN_SIZE: (f32, f32) = (640., 320.);
const SIDEBAR_WIDTH: f32 = 220.;
const THEME_LIST_WIDTH: f32 = 220.;
const THEME_LIST_HEIGHT: f32 = 180.;
const ACTIVITY_WIDTH: f32 = 380.;
/// Share of the display a new window may take up.
const MAX_DISPLAY_SHARE: f32 = 0.85;
/// Below these window widths the activity log, then the channel
/// descriptions and the title, make way for the rest.
const ACTIVITY_MIN_WIDTH: f32 = 1200.;
const DETAIL_MIN_WIDTH: f32 = 950.;
/// Width of the strips along the window edges that resize it.
const RESIZE_BORDER: f32 = 5.;
const RESIZE_CORNER: f32 = 14.;

/// Choices that are remembered in the channel database between runs.
struct Settings {
    volume: f32,
    squelch_db: f32,
    /// Airspy linearity gain, 0-21.
    gain: u8,
    /// Take turns between bands when the selected systems don't fit in one
    /// tuning.
    multiband: bool,
    /// Seconds on a quiet band before moving on.
    dwell_secs: f32,
    /// Longest turn, in seconds, for a band that stays busy.
    max_stay_secs: f32,
    /// Keep recordings after the app closes.
    save_recordings: bool,
    /// Name of the colour theme: a file in the themes directory, or the
    /// built-in one.
    theme: String,
}

impl Settings {
    fn load(db: Option<&Db>) -> Self {
        fn get<T: FromStr>(db: Option<&Db>, key: &str, default: T) -> T {
            db.and_then(|db| db.setting(key))
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        let defaults = Config::default();
        Self {
            volume: get(db, "volume", 3.0),
            squelch_db: get(db, "squelch_db", 6.0),
            gain: get(db, "gain", defaults.gain),
            multiband: get(db, "multiband", false),
            dwell_secs: get(db, "dwell_secs", defaults.dwell_secs),
            max_stay_secs: get(db, "max_stay_secs", defaults.max_stay_secs),
            save_recordings: get(db, "save_recordings", false),
            theme: get(db, "theme", theme::DEFAULT.to_string()),
        }
    }

    fn save(&self, db: &Db) {
        let values = [
            ("volume", self.volume.to_string()),
            ("squelch_db", self.squelch_db.to_string()),
            ("gain", self.gain.to_string()),
            ("multiband", self.multiband.to_string()),
            ("dwell_secs", self.dwell_secs.to_string()),
            ("max_stay_secs", self.max_stay_secs.to_string()),
            ("save_recordings", self.save_recordings.to_string()),
            ("theme", self.theme.clone()),
        ];
        for (key, value) in values {
            db.set_setting(key, &value).ok();
        }
    }
}

/// Where recordings that are kept go: beside the channel database.
fn kept_recordings_dir() -> PathBuf {
    Db::default_path().with_file_name("recordings")
}

/// Where recordings go when they are only kept for replaying this session.
fn replay_dir() -> PathBuf {
    std::env::temp_dir().join(format!("airspy-scanner-replay-{}", std::process::id()))
}

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
    /// Something worth knowing that isn't an error, such as what an import
    /// brought in; shown until the next action.
    notice: Option<String>,
    settings: Settings,
    /// Showing the settings page in place of the channels.
    show_settings: bool,
    /// The themes on offer, as of when the settings page was opened.
    themes: Vec<String>,
    /// The theme list, while it is dropped down.
    theme_search: Option<ThemeSearch>,
    /// Keyboard focus for typing into the theme list.
    theme_focus: FocusHandle,
    /// The form for a system or channel, while one is being edited.
    editor: Option<editor::Editor>,
    editor_focus: FocusHandle,
    /// What the channel list and activity log are narrowed down to.
    filter: Filter,
    filter_focus: FocusHandle,
}

/// Which channels and calls to show.
#[derive(Default)]
struct Filter {
    /// Words that must all appear in a channel's number, name or description.
    query: String,
    /// Only channels with a transmission on them right now.
    active_only: bool,
    /// Only channels that have been heard since the scan started.
    heard_only: bool,
}

impl Filter {
    fn is_set(&self) -> bool {
        !self.query.trim().is_empty() || self.active_only || self.heard_only
    }

    /// Whether every word of the query is somewhere in `text`, in any
    /// letter case.
    fn matches(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        self.query
            .to_lowercase()
            .split_whitespace()
            .all(|word| text.contains(word))
    }
}

/// What has been typed into the open theme list, and where in it the
/// keyboard is.
#[derive(Default)]
struct ThemeSearch {
    query: String,
    /// Index into the themes that match the query.
    highlighted: usize,
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
            notice: None,
            settings: Settings::load(None),
            show_settings: false,
            themes: Vec::new(),
            theme_search: None,
            theme_focus: cx.focus_handle(),
            editor: None,
            editor_focus: cx.focus_handle(),
            filter: Filter::default(),
            filter_focus: cx.focus_handle(),
        };
        match Db::open(&Db::default_path()) {
            Ok(db) => view.db = Some(db),
            Err(e) => view.error = Some(e),
        }
        view.settings = Settings::load(view.db.as_ref());
        if let Err(e) = theme::select(&view.settings.theme) {
            view.error = Some(e);
        }
        // Asked once, up front: the Airspy can't be queried while in use.
        match engine::sample_rate(&Config::default()) {
            Ok(rate) => view.rate = Some(rate),
            Err(e) => view.error = Some(e),
        }
        // Pick up where the last run left off, or with the first system.
        let saved = view
            .db
            .as_ref()
            .and_then(|db| db.setting("selected"))
            .unwrap_or_default();
        view.selected = saved.split(',').filter_map(|id| id.parse().ok()).collect();
        view.reload_systems();
        if view.selected.is_empty() {
            view.selected = view.systems.first().map(|s| s.id).into_iter().collect();
        }
        if view.error.is_none() {
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
            std::fs::remove_dir_all(replay_dir()).ok();
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
                    gain: self.settings.gain,
                    multiband: self.settings.multiband,
                    dwell_secs: self.settings.dwell_secs,
                    max_stay_secs: self.settings.max_stay_secs,
                    // Always recorded, so calls can be replayed from the log;
                    // channels marked for recording are always kept.
                    record: Some(self.recordings_dir()),
                    record_marked: Some(kept_recordings_dir()),
                    ..Config::default()
                };
                let (volume, squelch_db) = (self.settings.volume, self.settings.squelch_db);
                self.session = Some(Session::start(plan, config, volume, squelch_db));
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Where this run's recordings go: beside the channel database if they
    /// are being kept, otherwise somewhere that is cleared on exit.
    fn recordings_dir(&self) -> PathBuf {
        if self.settings.save_recordings {
            kept_recordings_dir()
        } else {
            replay_dir()
        }
    }

    /// Add a system to the scan or take it out. Without multi-band
    /// scanning, systems too far apart in frequency to share one tuning
    /// replace the selection instead.
    fn toggle_system(&mut self, id: i64) {
        self.notice = None;
        if let Some(i) = self.selected.iter().position(|s| *s == id) {
            self.selected.remove(i);
        } else {
            let mut both = self.selected.clone();
            both.push(id);
            let fits = self.plan(&both).is_ok_and(|plan| {
                let (lo, hi, _) = plan.span();
                self.rate.is_some_and(|rate| hi - lo <= engine::usable_span_hz(rate))
            });
            self.selected = if fits || self.settings.multiband {
                both
            } else {
                vec![id]
            };
        }
        self.remember_selection();
        self.start();
    }

    fn remember_selection(&self) {
        if let Some(db) = &self.db {
            let ids: Vec<String> = self.selected.iter().map(i64::to_string).collect();
            db.set_setting("selected", &ids.join(",")).ok();
        }
    }

    /// Change settings, remember them, and restart the scan if `restart`
    /// (for the ones the receiver only reads when it starts).
    fn update_settings(&mut self, restart: bool, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        if let Some(db) = &self.db {
            self.settings.save(db);
        }
        if restart {
            self.start();
        }
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
        self.update_settings(false, |s| s.volume = volume.clamp(0.5, 10.0));
        if let Some(s) = &self.session {
            s.controls.set_volume(self.settings.volume);
        }
    }

    fn set_squelch(&mut self, db: f32) {
        self.update_settings(false, |s| s.squelch_db = db.clamp(3.0, 30.0));
        if let Some(s) = &self.session {
            s.controls.set_squelch_db(self.settings.squelch_db);
        }
    }

    fn header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Wayland compositors like GNOME's draw no title bar for us, so this
        // bar is one: drag to move, double-click to maximize.
        let own_frame = matches!(window.window_decorations(), Decorations::Client { .. });
        let roomy = window.bounds().size.width >= px(DETAIL_MIN_WIDTH);
        let muted = self.session.as_ref().is_some_and(|s| s.controls.muted());
        let (volume, squelch_db) = (self.settings.volume, self.settings.squelch_db);
        let show_settings = self.show_settings;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .bg(rgb(theme().panel))
            .border_b_1()
            .border_color(rgb(theme().border))
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
                button("squelch-down", "−", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_squelch(squelch_db - 1.0);
                    cx.notify();
                })),
                button("squelch-up", "+", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_squelch(squelch_db + 1.0);
                    cx.notify();
                })),
            ))
            .child(stepper(
                "Volume",
                format!("{volume:.1}"),
                button("volume-down", "−", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_volume(volume - 0.5);
                    cx.notify();
                })),
                button("volume-up", "+", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_volume(volume + 0.5);
                    cx.notify();
                })),
            ))
            .child(
                button("mute", if muted { "Muted" } else { "Mute" }, muted, theme().error).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(s) = &this.session {
                            s.controls.set_muted(!muted);
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                button("settings", "Settings", show_settings, theme().accent).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.show_settings = !show_settings;
                        this.editor = None;
                        this.theme_search = None;
                        this.themes = theme::available();
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
                        .child(
                            button("minimize", "–", false, theme().accent)
                                .on_click(|_, window, _| window.minimize_window()),
                        )
                        .child(
                            button("maximize", "□", false, theme().accent)
                                .on_click(|_, window, _| window.zoom_window()),
                        )
                        .child(
                            button("close", "✕", false, theme().error).on_click(|_, window, _| window.remove_window()),
                        ),
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
            .border_color(rgb(theme().border))
            .bg(rgb(theme().panel));
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
                    .flex()
                    .items_center()
                    .gap_1()
                    .mx_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .when(selected, |item| item.bg(rgb(theme().raised)))
                    .child(
                        div()
                            .id(("system", id as usize))
                            .flex_1()
                            .overflow_hidden()
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.8))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_system(id);
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .text_color(rgb(if selected { theme().accent } else { theme().text }))
                                    .child(system.name.clone()),
                            )
                            .child(div().text_xs().truncate().text_color(rgb(theme().muted)).child(summary)),
                    )
                    .child(
                        button(("edit-system", id as usize), "Edit", false, theme().accent).on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.edit_system(Some(id), window);
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
        list.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .px_4()
                .py_3()
                .child(
                    button("radioreference", "RadioReference…", false, theme().accent).on_click(cx.listener(
                        |this, _, window, cx| {
                            this.import_from_radioreference(window);
                            cx.notify();
                        },
                    )),
                )
                .child(
                    button("new-system", "New…", false, theme().accent).on_click(cx.listener(|this, _, window, cx| {
                        this.edit_system(None, window);
                        cx.notify();
                    })),
                )
                .child(
                    button("import", "Import…", false, theme().accent).on_click(cx.listener(|_, _, _, cx| {
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
            (Some(error), ..) => ("Receiver stopped".to_string(), error.clone(), theme().error),
            (None, ..) if session.controls.replaying() => (
                "Replaying".to_string(),
                "A recorded call from the activity log".to_string(),
                theme().priority,
            ),
            (None, Some(c), _) => {
                let e = &session.plan.entries[c];
                (e.tag.clone(), format!("{}  ·  {}", e.desc, e.id()), theme().accent)
            }
            (None, None, Some(c)) => {
                let e = &session.plan.entries[c];
                (
                    e.tag.clone(),
                    format!("Holding on {}, waiting for traffic", e.id()),
                    theme().hold,
                )
            }
            (None, None, None) => {
                let (lo, hi, _) = session.plan.span();
                let detail = match live.band {
                    Some((index, count, lo, hi)) => format!(
                        "{} channels  ·  band {} of {}, {:.1}–{:.1} MHz",
                        session.plan.entries.len(),
                        index + 1,
                        count,
                        lo / 1e6,
                        hi / 1e6
                    ),
                    None => format!(
                        "{} channels, {:.1}–{:.1} MHz",
                        session.plan.entries.len(),
                        lo / 1e6,
                        hi / 1e6
                    ),
                };
                ("Scanning".to_string(), detail, theme().muted)
            }
        };
        div()
            .flex_none()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(theme().border))
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(color))
                    .child(title),
            )
            .child(div().text_sm().text_color(rgb(theme().muted)).child(detail))
    }

    fn channel_row(&self, c: usize, roomy: bool, session: &Session, live: &Live, cx: &mut Context<Self>) -> Div {
        let e = &session.plan.entries[c];
        let skipped = session.controls.skipped(c);
        let pinned = session.controls.pinned() == Some(c);
        let priority = session.controls.priority(c);
        let recorded = session.controls.recorded(c);
        let (open, playing) = (live.open[c], live.playing == Some(c));
        let level = (live.levels[c] / METER_FULL_DB).clamp(0.0, 1.0);
        let color = if playing {
            theme().accent
        } else if open {
            theme().text
        } else {
            theme().muted
        };

        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_1()
            .text_sm()
            .when(playing, |row| row.bg(rgb(theme().raised)))
            .when(skipped, |row| row.opacity(0.4))
            .child(div().flex_none().size_2().rounded_full().bg(rgb(if open {
                theme().accent
            } else {
                theme().border
            })))
            .child(
                div()
                    .flex_none()
                    .w(px(70.))
                    .text_color(rgb(theme().muted))
                    .child(e.id()),
            )
            .child(
                div()
                    .when(roomy, |tag| tag.flex_none().w(px(130.)))
                    .when(!roomy, |tag| tag.flex_1())
                    .truncate()
                    .text_color(rgb(color))
                    .child(e.tag.clone()),
            )
            .when(roomy, |row| {
                row.child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_color(rgb(theme().muted))
                        .child(e.desc.clone()),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(70.))
                    .h_1()
                    .rounded_full()
                    .bg(rgb(theme().raised))
                    .child(div().h_full().w(relative(level)).rounded_full().bg(rgb(if open {
                        theme().accent
                    } else {
                        theme().muted
                    }))),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(40.))
                    .text_color(rgb(theme().muted))
                    .child(match live.calls[c] {
                        0 => String::new(),
                        n => format!("×{n}"),
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(62.))
                    .text_color(rgb(theme().muted))
                    .child(live.last_heard[c].clone().unwrap_or_default()),
            )
            .child(
                button(("record", c), "Rec", recorded, theme().error).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = &this.session {
                        // A channel worth recording is one not to miss: it
                        // becomes a priority channel too (which can then be
                        // turned off on its own).
                        s.controls.set_recorded(c, !recorded);
                        if !recorded {
                            s.controls.set_priority(c, true);
                            s.controls.set_skipped(c, false);
                        }
                        if let (Some(db), Some(channel)) = (&this.db, s.plan.entries[c].channel_id) {
                            db.set_record(channel, !recorded).ok();
                            if !recorded {
                                db.set_skip(channel, false).ok();
                            }
                        }
                    }
                    cx.notify();
                })),
            )
            .child(
                button(("priority", c), "Pri", priority, theme().priority).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(s) = &this.session {
                            s.controls.set_priority(c, !priority);
                            // Priorities are remembered with the channel.
                            if let (Some(db), Some(channel)) = (&this.db, s.plan.entries[c].channel_id) {
                                db.set_priority(channel, !priority).ok();
                            }
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                button(("hold", c), "Hold", pinned, theme().hold).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = &this.session {
                        s.controls.set_pinned((!pinned).then_some(c));
                        s.controls.set_skipped(c, false);
                    }
                    cx.notify();
                })),
            )
            .child(
                button(("skip", c), "Skip", skipped, theme().error).on_click(cx.listener(move |this, _, _, cx| {
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
            // The catch-all entry isn't a channel in the database.
            .child(match e.channel_id {
                Some(channel) => button(("edit", c), "Edit", false, theme().accent).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.edit_channel(0, Some(channel), window);
                        cx.notify();
                    },
                )),
                None => div().id(("no-edit", c)).flex_none().w(px(38.)),
            })
    }

    /// Whether the filter lets a channel be shown.
    fn shows_channel(&self, c: usize, session: &Session, live: &Live) -> bool {
        let e = &session.plan.entries[c];
        (!self.filter.active_only || live.open[c])
            && (!self.filter.heard_only || live.calls[c] > 0)
            && self.filter.matches(&format!("{} {} {}", e.id(), e.tag, e.desc))
    }

    /// Whether the filter's words let a call in the activity log be shown.
    fn shows_call(&self, call: &session::Call, session: &Session) -> bool {
        let e = &session.plan.entries[call.channel];
        let talkgroup = call.talkgroup.map_or(String::new(), |tg| format!("talkgroup {tg}"));
        let unit = call.unit.map_or(String::new(), |unit| format!("unit {unit}"));
        self.filter
            .matches(&format!("{} {} {} {talkgroup} {unit}", e.id(), e.tag, e.desc))
    }

    /// The search box and toggles above the channel list. `shown` of
    /// `total` channels pass the filter.
    fn filter_bar(&self, shown: usize, total: usize, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let typing = self.filter_focus.is_focused(window);
        let (active_only, heard_only) = (self.filter.active_only, self.filter.heard_only);
        let query = self.filter.query.clone();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(theme().border))
            .child(
                div()
                    .id("filter")
                    .track_focus(&self.filter_focus)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            // Escape clears the search and gives the keyboard back.
                            "escape" => {
                                this.filter.query.clear();
                                window.blur();
                            }
                            "enter" => window.blur(),
                            _ => {
                                if !editor::type_into(&mut this.filter.query, event, cx) {
                                    return;
                                }
                            }
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.filter_focus);
                        cx.notify();
                    }))
                    .flex_1()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if typing { theme().accent } else { theme().border }))
                    .bg(rgb(theme().panel))
                    .text_sm()
                    .cursor_text()
                    .child(match (query.is_empty(), typing) {
                        (true, false) => div()
                            .text_color(rgb(theme().muted))
                            .child("Search channels and calls: name, frequency, talkgroup, unit…"),
                        (_, true) => div().child(format!("{query}▏")),
                        (false, false) => div().child(query),
                    }),
            )
            .child(
                button("filter-active", "Active", active_only, theme().accent).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.filter.active_only = !active_only;
                        cx.notify();
                    },
                )),
            )
            .child(
                button("filter-heard", "Heard", heard_only, theme().accent).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.filter.heard_only = !heard_only;
                        cx.notify();
                    },
                )),
            )
            .when(self.filter.is_set(), |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .text_sm()
                        .text_color(rgb(theme().muted))
                        .child(format!("{shown} of {total}")),
                )
                .child(
                    button("filter-clear", "Clear", false, theme().accent).on_click(cx.listener(|this, _, _, cx| {
                        this.filter = Filter::default();
                        cx.notify();
                    })),
                )
            })
    }

    fn activity(&self, session: &Session, live: &Live, cx: &mut Context<Self>) -> impl IntoElement {
        let replaying = session.controls.replaying();
        div()
            .id("activity")
            .flex_none()
            .w(px(ACTIVITY_WIDTH))
            .h_full()
            .overflow_y_scroll()
            .border_l_1()
            .border_color(rgb(theme().border))
            .bg(rgb(theme().panel))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pr_4()
                    .child(section_title("Activity"))
                    .when(replaying, |title| {
                        title.child(
                            button("stop-replay", "Stop replay", true, theme().priority).on_click(cx.listener(
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
            .when(live.log.is_empty(), |log| {
                log.child(
                    div()
                        .px_4()
                        .text_sm()
                        .text_color(rgb(theme().muted))
                        .child("Nothing heard yet."),
                )
            })
            .children(
                live.log
                    .iter()
                    .enumerate()
                    .filter(|(_, call)| self.shows_call(call, session))
                    .map(|(i, call)| {
                        let entry = &session.plan.entries[call.channel];
                        let mut who = match (&entry.kind, call.talkgroup) {
                            (Kind::OtherTalkgroups { .. }, Some(talkgroup)) => format!("Talkgroup {talkgroup}"),
                            (_, _) => entry.tag.clone(),
                        };
                        if let Some(unit) = call.unit {
                            who.push_str(&format!("  ·  unit {unit}"));
                        }
                        // Finished calls can be played back from their recording.
                        let replay = match &call.recording {
                            Some(recording) => {
                                let recording = recording.clone();
                                button(("replay", i), "▶", false, theme().priority).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(s) = &this.session {
                                            s.controls.replay(recording.clone());
                                        }
                                        cx.notify();
                                    },
                                ))
                            }
                            None => div().id(("no-replay", i)).flex_none().w(px(22.)),
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_1()
                            .text_sm()
                            .child(replay)
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(theme().muted))
                                    .child(call.time.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .text_color(rgb(if call.played { theme().text } else { theme().muted }))
                                    .child(who),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(theme().muted))
                                    .child(format!("{:+.0} dB", call.snr_db)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(34.))
                                    .text_color(rgb(theme().muted))
                                    .child(match call.secs {
                                        Some(secs) => format!("{secs:.0}s"),
                                        None => "live".into(),
                                    }),
                            )
                    }),
            )
    }

    /// Themes whose names contain what has been typed, in any letter case.
    fn matching_themes(&self) -> Vec<String> {
        let query = self
            .theme_search
            .as_ref()
            .map(|s| s.query.to_lowercase())
            .unwrap_or_default();
        self.themes
            .iter()
            .filter(|name| name.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    fn choose_theme(&mut self, name: String) {
        // A theme that can't be read falls back to the default.
        self.error = theme::select(&name).err();
        let chosen = if self.error.is_none() {
            name
        } else {
            theme::DEFAULT.into()
        };
        self.update_settings(false, |s| s.theme = chosen);
        self.theme_search = None;
    }

    /// Typing while the theme list is open: letters narrow it down, the
    /// arrows move through it, Enter picks and Escape closes.
    fn theme_search_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let matches = self.matching_themes();
        let Some(search) = &mut self.theme_search else { return };
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" => self.theme_search = None,
            "enter" => {
                if let Some(name) = matches.get(search.highlighted) {
                    self.choose_theme(name.clone());
                }
            }
            "down" => search.highlighted = (search.highlighted + 1).min(matches.len().saturating_sub(1)),
            "up" => search.highlighted = search.highlighted.saturating_sub(1),
            "backspace" => {
                search.query.pop();
                search.highlighted = 0;
            }
            _ => {
                let shortcut = keystroke.modifiers.control || keystroke.modifiers.platform || keystroke.modifiers.alt;
                let Some(typed) = keystroke.key_char.as_ref().filter(|_| !shortcut) else {
                    return;
                };
                search.query.push_str(typed);
                search.highlighted = 0;
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The theme setting's control: the current theme, which drops down
    /// into a list that can be searched by typing.
    fn theme_picker(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let current = self.settings.theme.clone();
        let picker = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(THEME_LIST_WIDTH))
            .gap_1()
            .child(
                div()
                    .id("theme-current")
                    .flex()
                    .justify_between()
                    .px_2()
                    .rounded_md()
                    .text_sm()
                    .bg(rgb(theme().raised))
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.8))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.theme_search = match this.theme_search {
                            Some(_) => None,
                            None => {
                                this.themes = theme::available();
                                window.focus(&this.theme_focus);
                                Some(ThemeSearch::default())
                            }
                        };
                        cx.notify();
                    }))
                    .child(div().truncate().child(current.clone()))
                    .child(div().flex_none().text_color(rgb(theme().muted)).child("▾")),
            );
        let Some(search) = &self.theme_search else {
            return picker.into_any_element();
        };
        let matches = self.matching_themes();
        let highlighted = search.highlighted.min(matches.len().saturating_sub(1));
        picker
            .track_focus(&self.theme_focus)
            .on_key_down(cx.listener(|this, event, _, cx| this.theme_search_key(event, cx)))
            .child(
                div()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(theme().accent))
                    .text_sm()
                    .child(if search.query.is_empty() {
                        div().text_color(rgb(theme().muted)).child("Type to search…")
                    } else {
                        div().child(format!("{}▏", search.query))
                    }),
            )
            .child(
                div()
                    .id("theme-list")
                    .max_h(px(THEME_LIST_HEIGHT))
                    .overflow_y_scroll()
                    .rounded_md()
                    .bg(rgb(theme().panel))
                    .border_1()
                    .border_color(rgb(theme().border))
                    .when(matches.is_empty(), |list| {
                        list.child(
                            div()
                                .px_2()
                                .text_sm()
                                .text_color(rgb(theme().muted))
                                .child("No themes match."),
                        )
                    })
                    .children(matches.into_iter().enumerate().map(|(i, name)| {
                        let chosen = name.clone();
                        div()
                            .id(("theme", i))
                            .px_2()
                            .text_sm()
                            .truncate()
                            .cursor_pointer()
                            .when(i == highlighted, |item| item.bg(rgb(theme().raised)))
                            .when(name == current, |item| item.text_color(rgb(theme().accent)))
                            .hover(|style| style.bg(rgb(theme().raised)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_theme(chosen.clone());
                                cx.notify();
                            }))
                            .child(name)
                    })),
            )
            .into_any_element()
    }

    fn settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = &self.settings;
        let (gain, multiband, dwell, stay, save) =
            (s.gain, s.multiband, s.dwell_secs, s.max_stay_secs, s.save_recordings);
        // One setting: its name, what it does, and the control for it.
        let row = |name: &'static str, about: String, control: gpui::AnyElement| {
            div()
                .flex()
                .items_center()
                .gap_4()
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(rgb(theme().border))
                .child(
                    div()
                        .flex_1()
                        .child(div().child(name))
                        .child(div().text_sm().text_color(rgb(theme().muted)).child(about)),
                )
                .child(control)
        };
        let toggle = |id: &'static str, on: bool| button(id, if on { "On" } else { "Off" }, on, theme().accent);

        div()
            .id("settings-page")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(section_title("Settings"))
            .when_some(self.error.clone(), |page, error| {
                page.child(div().px_4().py_2().text_sm().text_color(rgb(theme().error)).child(error))
            })
            .child(row(
                "Multi-band scanning",
                "Scan systems that are too far apart to receive at once by taking turns on each band. \
                 Calls on the bands not being listened to at that moment are missed."
                    .into(),
                toggle("multiband", multiband)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(true, |s| s.multiband = !multiband);
                        cx.notify();
                    }))
                    .into_any_element(),
            ))
            .child(row(
                "Time on a quiet band",
                "How long to listen to a band with no traffic before moving to the next.".into(),
                stepper(
                    "",
                    format!("{dwell:.1} s"),
                    button("dwell-down", "−", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(true, |s| s.dwell_secs = (dwell - 0.5).clamp(0.5, 10.0));
                        cx.notify();
                    })),
                    button("dwell-up", "+", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(true, |s| s.dwell_secs = (dwell + 0.5).clamp(0.5, 10.0));
                        cx.notify();
                    })),
                )
                .into_any_element(),
            ))
            .child(row(
                "Longest turn on a busy band",
                "A band with constant traffic is left after this long, between transmissions, \
                 so the others get a turn."
                    .into(),
                stepper(
                    "",
                    format!("{stay:.0} s"),
                    button("stay-down", "−", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(true, |s| s.max_stay_secs = (stay - 5.0).clamp(5.0, 120.0));
                        cx.notify();
                    })),
                    button("stay-up", "+", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(true, |s| s.max_stay_secs = (stay + 5.0).clamp(5.0, 120.0));
                        cx.notify();
                    })),
                )
                .into_any_element(),
            ))
            .child(row(
                "Receiver gain",
                "Airspy gain, 0 to 21. Raise it for weak signals; lower it if strong ones cause noise on other channels."
                    .into(),
                stepper(
                    "",
                    gain.to_string(),
                    button("gain-down", "−", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(true, |s| s.gain = gain.saturating_sub(1));
                        cx.notify();
                    })),
                    button("gain-up", "+", false, theme().accent).on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(true, |s| s.gain = (gain + 1).min(21));
                        cx.notify();
                    })),
                )
                .into_any_element(),
            ))
            .child(row(
                "Keep recordings",
                format!(
                    "Every call is recorded so it can be replayed from the activity log. On: all kept in {}. \
                     Off: deleted when the app closes, except on channels marked Rec.",
                    kept_recordings_dir().display()
                ),
                toggle("save-recordings", save)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(true, |s| s.save_recordings = !save);
                        cx.notify();
                    }))
                    .into_any_element(),
            ))
            .child(row(
                "Theme",
                format!(
                    "Colours for the app. Add your own as .json files in {}.",
                    theme::dir().display()
                ),
                self.theme_picker(cx),
            ))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .text_sm()
                    .text_color(rgb(theme().muted))
                    .child(format!("Channel database: {}", Db::default_path().display())),
            )
    }
}

impl Render for ScannerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.bounds().size.width;
        let roomy = width >= px(DETAIL_MIN_WIDTH);
        let main = div().flex_1().h_full().flex().flex_col().overflow_hidden();
        let main = match &self.session {
            _ if self.editor.is_some() => main.child(self.editor_page(self.editor.as_ref().unwrap(), cx)),
            _ if self.show_settings => main.child(self.settings_page(cx)),
            None => {
                let message = match (&self.error, self.systems.is_empty()) {
                    (Some(error), _) => div().text_color(rgb(theme().error)).child(error.clone()),
                    (None, true) => div()
                        .text_color(rgb(theme().muted))
                        .child("The channel database is empty. Import a channel file to begin."),
                    (None, false) => div()
                        .text_color(rgb(theme().muted))
                        .child("Pick a system on the left to scan it."),
                };
                main.child(message.p_4())
                    .when_some(self.notice.clone(), |main, notice| {
                        main.child(div().px_4().text_sm().text_color(rgb(theme().muted)).child(notice))
                    })
            }
            Some(session) => {
                let live = session.live.lock().unwrap();
                let total = session.plan.entries.len();
                let shown: Vec<usize> = (0..total).filter(|&c| self.shows_channel(c, session, &live)).collect();
                main.child(self.now_playing(session, &live))
                    .when_some(self.error.clone(), |main, error| {
                        main.child(
                            div()
                                .flex_none()
                                .px_4()
                                .py_2()
                                .text_sm()
                                .text_color(rgb(theme().error))
                                .child(error),
                        )
                    })
                    .when_some(self.notice.clone(), |main, notice| {
                        main.child(
                            div()
                                .flex_none()
                                .px_4()
                                .py_2()
                                .text_sm()
                                .text_color(rgb(theme().muted))
                                .child(notice),
                        )
                    })
                    .child(self.filter_bar(shown.len(), total, window, cx))
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
                                    .when(shown.is_empty(), |list| {
                                        list.child(
                                            div()
                                                .px_4()
                                                .text_sm()
                                                .text_color(rgb(theme().muted))
                                                .child("No channels match the filter."),
                                        )
                                    })
                                    .children(shown.iter().map(|&c| self.channel_row(c, roomy, session, &live, cx))),
                            )
                            .when(width >= px(ACTIVITY_MIN_WIDTH), |body| {
                                body.child(self.activity(session, &live, cx))
                            }),
                    )
            }
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme().bg))
            .text_color(rgb(theme().text))
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
        .text_color(rgb(theme().muted))
        .child(title.into())
}

/// A small push button; `active` fills it with `color`.
fn button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, active: bool, color: u32) -> Stateful<Div> {
    let (bg, fg): (Rgba, Rgba) = if active {
        (rgb(color), rgb(theme().bg))
    } else {
        (rgb(theme().raised), rgb(theme().text))
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
        .child(label.into())
}

fn stepper(label: &'static str, value: String, down: Stateful<Div>, up: Stateful<Div>) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_sm()
        .child(div().text_color(rgb(theme().muted)).child(label))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_needs_every_word() {
        let filter = |query: &str| Filter {
            query: query.into(),
            ..Filter::default()
        };
        let channel = "488.3125 San Mateo PD1 San Mateo Police Dispatch";
        assert!(filter("").matches(channel));
        assert!(filter("  ").matches(channel));
        assert!(filter("mateo DISPATCH").matches(channel));
        assert!(filter("488.3").matches(channel));
        assert!(!filter("mateo fire").matches(channel));
        assert!(!filter("").is_set() && filter("x").is_set());
    }
}
