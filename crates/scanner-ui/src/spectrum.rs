//! The spectrum each receiver picks up: a trace of the latest power across
//! its band above a waterfall of the last few seconds, with a line where
//! each channel is.

use gpui::{
    Bounds, IntoElement, ParentElement, PathBuilder, Rgba, Styled, Window, canvas, div, fill, point, px, relative, rgb,
    size,
};
use scanner::plan::Kind;
use scanner::session::{Live, Session, SpectrumRow};

use crate::theme::{Theme, theme};

/// Share of the height the trace takes; the waterfall has the rest.
const TRACE_SHARE: f32 = 0.38;
/// Smallest waterfall cell, in pixels, so a big window doesn't mean tens of
/// thousands of them.
const MIN_CELL_WIDTH: f32 = 3.;
const MIN_CELL_HEIGHT: f32 = 2.;
/// Power shown from just under the noise floor up to this much above it.
const RANGE_DB: f32 = 45.;
const BELOW_FLOOR_DB: f32 = 3.;

/// What a line on the spectrum stands for.
#[derive(Clone, Copy)]
enum Mark {
    Playing,
    Open,
    Idle,
    Skipped,
    /// One of a trunked system's frequencies.
    Trunked,
}

/// A spectrum panel `height` pixels high per receiver that has reported one.
pub fn panels(session: &Session, live: &Live, height: f32) -> Vec<gpui::AnyElement> {
    let several = live.receivers.len() > 1;
    let plan = &session.plan;
    let mut marks: Vec<(f64, Mark)> = plan
        .entries
        .iter()
        .enumerate()
        .filter_map(|(c, e)| match e.kind {
            Kind::Analog { freq_hz, .. } | Kind::Digital { freq_hz, .. } => {
                let mark = if live.playing == Some(c) {
                    Mark::Playing
                } else if live.open.get(c) == Some(&true) {
                    Mark::Open
                } else if session.controls.skipped(c) {
                    Mark::Skipped
                } else {
                    Mark::Idle
                };
                Some((freq_hz, mark))
            }
            Kind::Talkgroup { .. } | Kind::OtherTalkgroups { .. } => None,
        })
        .collect();
    marks.extend(plan.p25.iter().map(|f| (f.freq_hz, Mark::Trunked)));
    // The loudest kinds go last, so they are drawn on top.
    marks.sort_by_key(|&(_, mark)| match mark {
        Mark::Trunked | Mark::Skipped => 0,
        Mark::Idle => 1,
        Mark::Open => 2,
        Mark::Playing => 3,
    });

    live.receivers
        .iter()
        .filter_map(|receiver| {
            let newest = receiver.spectrum.front()?;
            let (lo_hz, hi_hz) = (newest.lo_hz, newest.hi_hz);
            let rows: Vec<SpectrumRow> = receiver.spectrum.iter().cloned().collect();
            let floor = noise_floor(&newest.db);
            let shown: Vec<(f64, Mark)> = marks
                .iter()
                .copied()
                .filter(|&(f, _)| f >= lo_hz && f <= hi_hz)
                .collect();
            // The channels on the air are named above their lines.
            let labels: Vec<(f32, String, u32)> = plan
                .entries
                .iter()
                .enumerate()
                .filter(|&(c, _)| live.playing == Some(c) || live.open.get(c) == Some(&true))
                .filter_map(|(c, e)| match e.kind {
                    Kind::Analog { freq_hz, .. } | Kind::Digital { freq_hz, .. }
                        if freq_hz >= lo_hz && freq_hz <= hi_hz =>
                    {
                        let color = if live.playing == Some(c) {
                            theme().accent
                        } else {
                            theme().text
                        };
                        Some((((freq_hz - lo_hz) / (hi_hz - lo_hz)) as f32, e.tag.clone(), color))
                    }
                    _ => None,
                })
                .collect();
            let caption = match several {
                true => format!("{}  ·  {:.3}–{:.3} MHz", receiver.name, lo_hz / 1e6, hi_hz / 1e6),
                false => format!("{:.3}–{:.3} MHz", lo_hz / 1e6, hi_hz / 1e6),
            };
            let colors = theme();
            Some(
                div()
                    .flex_none()
                    .h(px(height))
                    .relative()
                    .overflow_hidden()
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .bg(rgb(colors.bg))
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, (), window, _| {
                                paint(bounds, window, &rows, lo_hz, hi_hz, floor, &shown, &colors);
                            },
                        )
                        .size_full(),
                    )
                    .children(labels.into_iter().map(|(at, tag, color)| {
                        div()
                            .absolute()
                            .top(px(1.))
                            .left(relative(at))
                            .ml(px(3.))
                            .text_xs()
                            .text_color(rgb(color))
                            .child(tag)
                    }))
                    .child(
                        div()
                            .absolute()
                            .bottom(px(2.))
                            .right(px(6.))
                            .px_1()
                            .text_xs()
                            .bg(rgb(colors.panel))
                            .text_color(rgb(colors.muted))
                            .child(caption),
                    )
                    .into_any_element(),
            )
        })
        .collect()
}

/// A level most of the band sits at: what is there when nothing is.
fn noise_floor(db: &[f32]) -> f32 {
    let mut sorted = db.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted.get(sorted.len() / 4).copied().unwrap_or(-100.0)
}

#[allow(clippy::too_many_arguments)]
fn paint(
    bounds: Bounds<gpui::Pixels>,
    window: &mut Window,
    rows: &[SpectrumRow],
    lo_hz: f64,
    hi_hz: f64,
    floor: f32,
    marks: &[(f64, Mark)],
    colors: &Theme,
) {
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let span = hi_hz - lo_hz;
    if width < 1. || height < 1. || span <= 0. {
        return;
    }
    let origin = bounds.origin;
    let at = |x: f32, y: f32| point(origin.x + px(x), origin.y + px(y));
    let bottom_db = floor - BELOW_FLOOR_DB;
    let level = |db: f32| ((db - bottom_db) / RANGE_DB).clamp(0., 1.);
    let trace_height = (height * TRACE_SHARE).round();
    let fall_height = height - trace_height;

    // Waterfall, newest at the top. Every row is placed by its own span, so
    // rows from another band (when taking turns) fall where they belong.
    let columns = ((width / MIN_CELL_WIDTH) as usize).clamp(1, scanner::engine::SPECTRUM_COLUMNS);
    let cell_width = width / columns as f32;
    let shown_rows = ((fall_height / MIN_CELL_HEIGHT) as usize).clamp(1, rows.len().max(1));
    let cell_height = fall_height / shown_rows as f32;
    let (dark, mid, bright) = (rgb(colors.bg), rgb(colors.accent), rgb(colors.text));
    for (r, row) in rows.iter().take(shown_rows).enumerate() {
        let y = trace_height + r as f32 * cell_height;
        for x in 0..columns {
            let Some(db) = loudest(
                row,
                lo_hz + span * x as f64 / columns as f64,
                lo_hz + span * (x + 1) as f64 / columns as f64,
            ) else {
                continue;
            };
            let v = level(db);
            if v <= 0. {
                continue;
            }
            let color = if v < 0.6 {
                mix(dark, mid, v / 0.6)
            } else {
                mix(mid, bright, (v - 0.6) / 0.4)
            };
            window.paint_quad(fill(
                Bounds::new(
                    at(x as f32 * cell_width, y),
                    size(px(cell_width + 0.5), px(cell_height + 0.5)),
                ),
                color,
            ));
        }
    }

    // The latest power across the band.
    let newest = &rows[0];
    let points = (width as usize).clamp(2, newest.db.len() * 2);
    let mut trace = PathBuilder::stroke(px(1.25));
    let mut started = false;
    for i in 0..points {
        let from = lo_hz + span * i as f64 / points as f64;
        let to = lo_hz + span * (i + 1) as f64 / points as f64;
        let Some(db) = loudest(newest, from, to) else {
            continue;
        };
        let p = at(
            width * (i as f32 + 0.5) / points as f32,
            (trace_height - 2.) * (1. - level(db)) + 1.,
        );
        if started {
            trace.line_to(p);
        } else {
            trace.move_to(p);
            started = true;
        }
    }
    if let Ok(path) = trace.build() {
        window.paint_path(path, rgb(colors.accent));
    }
    window.paint_quad(fill(
        Bounds::new(at(0., trace_height - 0.5), size(px(width), px(1.))),
        rgb(colors.border),
    ));

    // A line at each channel.
    for &(freq_hz, mark) in marks {
        let x = (((freq_hz - lo_hz) / span) as f32 * width).round();
        let (color, alpha, line_width) = match mark {
            Mark::Playing => (colors.accent, 1.0, 2.),
            Mark::Open => (colors.text, 0.9, 1.5),
            Mark::Idle => (colors.muted, 0.55, 1.),
            Mark::Skipped => (colors.border, 0.8, 1.),
            Mark::Trunked => (colors.priority, 0.5, 1.),
        };
        let mut color = rgb(color);
        color.a = alpha;
        window.paint_quad(fill(
            Bounds::new(at(x - line_width / 2., 0.), size(px(line_width), px(height))),
            color,
        ));
    }
}

/// The highest power `row` has between two frequencies, if it covers them.
fn loudest(row: &SpectrumRow, from_hz: f64, to_hz: f64) -> Option<f32> {
    let (n, span) = (row.db.len(), row.hi_hz - row.lo_hz);
    if n == 0 || span <= 0. || to_hz <= row.lo_hz || from_hz >= row.hi_hz {
        return None;
    }
    let index = |f: f64| ((f - row.lo_hz) / span * n as f64).clamp(0., n as f64) as usize;
    let a = index(from_hz).min(n - 1);
    let b = index(to_hz).clamp(a + 1, n);
    row.db[a..b].iter().copied().reduce(f32::max)
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.,
    }
}
