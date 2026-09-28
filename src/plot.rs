//! Rendering of one tag's chart (all visible runs overlaid) plus its legend.
//! Draws into a plain `Buffer`, so it is shared by the TUI and snapshot mode.

use crate::store::Point;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::{self, Marker};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, BorderType, Borders, Chart, Dataset, GraphType, Row, Table, Widget};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------- colors

static TRUECOLOR: OnceLock<bool> = OnceLock::new();

pub fn init_colors(force_256: bool) {
    let tc = !force_256
        && std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
    let _ = TRUECOLOR.set(tc);
}

pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    if *TRUECOLOR.get_or_init(|| false) {
        return Color::Rgb(r, g, b);
    }
    // Nearest xterm-256 color (6x6x6 cube or grayscale ramp).
    const LV: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |c: u8| (0..6).min_by_key(|&i| (LV[i] as i32 - c as i32).abs()).unwrap();
    let (ri, gi, bi) = (near(r), near(g), near(b));
    let cube = (LV[ri], LV[gi], LV[bi]);
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gray_i = (((avg as i32 - 8).max(0)) / 10).min(23) as u8;
    let gv = 8 + gray_i as i32 * 10;
    let d = |a: (i32, i32, i32)| (a.0 - r as i32).pow(2) + (a.1 - g as i32).pow(2) + (a.2 - b as i32).pow(2);
    if d((gv, gv, gv)) < d((cube.0 as i32, cube.1 as i32, cube.2 as i32)) {
        Color::Indexed(232 + gray_i)
    } else {
        Color::Indexed(16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8)
    }
}

type Rgb = (u8, u8, u8);

/// Run palettes. The first two are colorblind-safe (Okabe & Ito 2008;
/// Paul Tol "bright"), with greys lightened for dark terminals.
pub const PALETTES: [(&str, &[Rgb]); 3] = [
    (
        "okabe-ito",
        &[
            (0xE6, 0x9F, 0x00), // orange
            (0x56, 0xB4, 0xE9), // sky blue
            (0x00, 0x9E, 0x73), // bluish green
            (0xF0, 0xE4, 0x42), // yellow
            (0x3D, 0x8F, 0xD6), // blue (lightened from #0072B2 for dark backgrounds)
            (0xD5, 0x5E, 0x00), // vermillion
            (0xCC, 0x79, 0xA7), // reddish purple
            (0xBB, 0xBB, 0xBB), // grey
        ],
    ),
    (
        "tol-bright",
        &[
            (0x44, 0x77, 0xAA),
            (0xEE, 0x66, 0x77),
            (0x22, 0x88, 0x33),
            (0xCC, 0xBB, 0x44),
            (0x66, 0xCC, 0xEE),
            (0xAA, 0x33, 0x77),
            (0xBB, 0xBB, 0xBB),
        ],
    ),
    (
        "vivid",
        &[
            (0x4e, 0xa8, 0xff),
            (0xff, 0x8c, 0x42),
            (0x5c, 0xd6, 0x7a),
            (0xff, 0x5c, 0x7a),
            (0xb4, 0x8c, 0xff),
            (0x4f, 0xd6, 0xd6),
            (0xff, 0xd1, 0x4f),
            (0xff, 0x7a, 0xd9),
            (0x9c, 0xc9, 0x4f),
            (0xc8, 0xa0, 0x78),
        ],
    ),
];

static PALETTE_IDX: AtomicUsize = AtomicUsize::new(0);

pub fn palette_name() -> &'static str {
    PALETTES[PALETTE_IDX.load(Ordering::Relaxed)].0
}

pub fn set_palette(name: &str) -> bool {
    match PALETTES.iter().position(|(n, _)| *n == name) {
        Some(i) => {
            PALETTE_IDX.store(i, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

pub fn cycle_palette() {
    PALETTE_IDX.store((PALETTE_IDX.load(Ordering::Relaxed) + 1) % PALETTES.len(), Ordering::Relaxed);
}

pub fn run_rgb(i: usize) -> Rgb {
    let p = PALETTES[PALETTE_IDX.load(Ordering::Relaxed)].1;
    p[i % p.len()]
}

/// Shape per run, so runs are distinguishable without relying on color.
const SYMBOLS: [char; 8] = ['●', '▲', '■', '◆', '▼', '✚', '✖', '★'];

pub fn run_symbol(i: usize) -> char {
    // offset by the palette cycle so (color, shape) pairs stay unique longer
    let n = PALETTES[PALETTE_IDX.load(Ordering::Relaxed)].1.len();
    SYMBOLS[(i + i / n) % SYMBOLS.len()]
}

fn dim((r, g, b): (u8, u8, u8)) -> Color {
    let m = |c: u8| ((c as u16 * 2 + 40 * 3) / 5) as u8;
    rgb(m(r), m(g), m(b))
}

pub fn accent() -> Color {
    rgb(0xff, 0x8c, 0x42)
}
pub fn muted() -> Color {
    rgb(0x80, 0x86, 0x90)
}
pub fn frame() -> Color {
    rgb(0x4a, 0x50, 0x5a)
}

// ---------------------------------------------------------------- formatting

pub fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    let a = v.abs();
    if a == 0.0 {
        return "0".into();
    }
    if !(1e-3..1e5).contains(&a) {
        return format!("{v:.2e}");
    }
    let decimals = (3 - a.log10().floor() as i32).clamp(0, 6) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

pub fn fmt_step(v: f64) -> String {
    let a = v.abs();
    if a < 1e4 {
        format!("{}", v.round() as i64)
    } else if a < 1e6 {
        format!("{}k", fmt_num(v / 1e3))
    } else if a < 1e9 {
        format!("{}M", fmt_num(v / 1e6))
    } else {
        format!("{}B", fmt_num(v / 1e9))
    }
}

pub fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        3600..=86399 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{:02}h", s / 86400, (s % 86400) / 3600),
    }
}

// ---------------------------------------------------------------- options

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XMode {
    Step,
    Relative,
}

#[derive(Clone, Debug)]
pub struct PlotOpts {
    pub smoothing: f64,
    pub log_y: bool,
    pub xmode: XMode,
    pub x_range: Option<(f64, f64)>,
    pub cursor: Option<f64>,
    pub ignore_outliers: bool,
    pub show_raw: bool,
    pub legend: bool,
    /// let the legend take as many rows as there are runs (snapshot mode)
    pub legend_full: bool,
    /// draw per-run shape markers on the lines
    pub markers: bool,
}

impl Default for PlotOpts {
    fn default() -> Self {
        PlotOpts {
            smoothing: 0.6,
            log_y: false,
            xmode: XMode::Step,
            x_range: None,
            cursor: None,
            ignore_outliers: false,
            show_raw: true,
            legend: true,
            legend_full: false,
            markers: true,
        }
    }
}

pub struct Series<'a> {
    pub name: &'a str,
    pub rgb: (u8, u8, u8),
    pub symbol: char,
    pub points: &'a [Point],
    pub first_wall: f64,
}

#[derive(Default, Clone, Copy)]
pub struct PlotInfo {
    /// x extent of all data
    pub x_full: (f64, f64),
    /// x extent currently shown
    pub x_view: (f64, f64),
    /// approximate screen rect of the plotting area (for mouse mapping)
    pub graph: Rect,
}

// ---------------------------------------------------------------- data prep

struct Prep {
    xs: Vec<f64>,
    raw: Vec<f64>,
    smooth: Vec<f64>,
}

fn x_of(p: &Point, mode: XMode, first_wall: f64) -> f64 {
    match mode {
        XMode::Step => p.step as f64,
        XMode::Relative => p.wall - first_wall,
    }
}

/// TensorBoard-style debiased exponential moving average.
pub fn ema(vals: &[f64], w: f64) -> Vec<f64> {
    if w <= 0.0 {
        return vals.to_vec();
    }
    let mut last = 0.0;
    let mut n = 0;
    vals.iter()
        .map(|&v| {
            if !v.is_finite() {
                return v;
            }
            last = last * w + (1.0 - w) * v;
            n += 1;
            last / (1.0 - w.powi(n))
        })
        .collect()
}

fn prep(s: &Series, o: &PlotOpts) -> Prep {
    let xs = s.points.iter().map(|p| x_of(p, o.xmode, s.first_wall)).collect();
    let raw: Vec<f64> = s.points.iter().map(|p| p.value).collect();
    let smooth = ema(&raw, o.smoothing);
    Prep { xs, raw, smooth }
}

/// Visible, downsampled, (log-)transformed points.
fn visible(xs: &[f64], ys: &[f64], view: (f64, f64), buckets: usize, log: bool) -> Vec<(f64, f64)> {
    let tf = |y: f64| if log { if y > 0.0 { Some(y.log10()) } else { None } } else { Some(y) };
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let n = xs.len();
    for i in 0..n {
        let x = xs[i];
        let inside = x >= view.0 && x <= view.1;
        // keep one neighbour on each side so lines reach the edges
        let near = (i + 1 < n && xs[i + 1] >= view.0 && x < view.0) || (i > 0 && xs[i - 1] <= view.1 && x > view.1);
        if (inside || near)
            && let Some(y) = tf(ys[i]).filter(|y| y.is_finite()) {
                pts.push((x, y));
            }
    }
    if pts.len() <= buckets * 4 || buckets == 0 {
        return pts;
    }
    // min/max per bucket keeps spikes visible
    let span = (view.1 - view.0).max(f64::EPSILON);
    let mut out = Vec::with_capacity(buckets * 2 + 2);
    let mut cur = usize::MAX;
    let (mut lo, mut hi) = ((0.0, f64::INFINITY), (0.0, f64::NEG_INFINITY));
    let flush = |out: &mut Vec<(f64, f64)>, lo: (f64, f64), hi: (f64, f64)| {
        if lo.1.is_finite() {
            if lo.0 <= hi.0 {
                out.push(lo);
                if hi != lo {
                    out.push(hi);
                }
            } else {
                out.push(hi);
                out.push(lo);
            }
        }
    };
    for &(x, y) in &pts {
        let b = (((x - view.0) / span) * buckets as f64).clamp(-1.0, buckets as f64) as i64 as usize;
        if b != cur {
            flush(&mut out, lo, hi);
            cur = b;
            lo = (x, y);
            hi = (x, y);
        } else {
            if y < lo.1 {
                lo = (x, y);
            }
            if y > hi.1 {
                hi = (x, y);
            }
        }
    }
    flush(&mut out, lo, hi);
    out
}

/// All (transformed) y values inside the x view, raw and/or smoothed.
fn in_view_values(preps: &[Prep], view: (f64, f64), with_raw: bool, log: bool) -> Vec<f64> {
    let mut out = Vec::new();
    for p in preps {
        for (i, &x) in p.xs.iter().enumerate() {
            if x < view.0 || x > view.1 {
                continue;
            }
            let vals = if with_raw { [Some(p.smooth[i]), Some(p.raw[i])] } else { [Some(p.smooth[i]), None] };
            for v in vals.into_iter().flatten() {
                let v = if log { if v > 0.0 { v.log10() } else { continue } } else { v };
                if v.is_finite() {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// q-quantile (0..=1) by selection, O(n).
fn quantile(v: &mut [f64], q: f64) -> f64 {
    let i = ((v.len() - 1) as f64 * q).round() as usize;
    *v.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1
}

fn nearest(xs: &[f64], x: f64) -> Option<usize> {
    if xs.is_empty() {
        return None;
    }
    let i = xs.partition_point(|&v| v < x);
    if i == 0 {
        Some(0)
    } else if i >= xs.len() {
        Some(xs.len() - 1)
    } else if (xs[i] - x).abs() < (x - xs[i - 1]).abs() {
        Some(i)
    } else {
        Some(i - 1)
    }
}

pub fn full_x_range(series: &[Series], mode: XMode) -> Option<(f64, f64)> {
    let mut r: Option<(f64, f64)> = None;
    for s in series {
        for p in [s.points.first(), s.points.last()].into_iter().flatten() {
            let x = x_of(p, mode, s.first_wall);
            r = Some(r.map_or((x, x), |(a, b)| (a.min(x), b.max(x))));
        }
    }
    r
}

// ---------------------------------------------------------------- drawing

pub fn draw_panel(buf: &mut Buffer, area: Rect, title: &str, series: &[Series], o: &PlotOpts, focused: bool) -> PlotInfo {
    let border = if focused { accent() } else { frame() };
    let mut flags = Vec::new();
    if o.smoothing > 0.0 {
        flags.push(format!("smooth {:.2}", o.smoothing));
    }
    if o.log_y {
        flags.push("log".into());
    }
    if o.xmode == XMode::Relative {
        flags.push("x: time".into());
    }
    if o.ignore_outliers {
        flags.push("no outliers".into());
    }
    if o.x_range.is_some() {
        flags.push("zoom".into());
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(if focused { accent() } else { rgb(0xe6, 0xe6, 0xe6) }).add_modifier(Modifier::BOLD),
        )))
        .title(Line::from(Span::styled(format!(" {} ", flags.join(" · ")), Style::default().fg(muted()))).right_aligned());
    let inner = block.inner(area);
    block.render(area, buf);

    let legend_h = if o.legend && inner.height >= 10 {
        let cap = if o.legend_full { inner.height.saturating_sub(6) } else { inner.height / 3 };
        (series.len() as u16 + 1).min(cap).max(2)
    } else {
        0
    };
    let [chart_area, legend_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(legend_h)]).areas(inner);

    let Some(x_full) = full_x_range(series, o.xmode) else {
        let msg = Line::from(Span::styled("no data", Style::default().fg(muted()))).centered();
        msg.render(Rect { y: chart_area.y + chart_area.height / 2, height: 1, ..chart_area }, buf);
        return PlotInfo::default();
    };
    let x_full = if x_full.0 == x_full.1 { (x_full.0 - 1.0, x_full.1 + 1.0) } else { x_full };
    let x_view = o.x_range.unwrap_or(x_full);

    let preps: Vec<Prep> = series.iter().map(|s| prep(s, o)).collect();
    let buckets = (chart_area.width as usize).saturating_sub(10) * 2;
    let with_raw = o.show_raw && o.smoothing > 0.0;
    let smooth_pts: Vec<Vec<(f64, f64)>> =
        preps.iter().map(|p| visible(&p.xs, &p.smooth, x_view, buckets, o.log_y)).collect();
    let raw_pts: Vec<Vec<(f64, f64)>> = if with_raw {
        preps.iter().map(|p| visible(&p.xs, &p.raw, x_view, buckets, o.log_y)).collect()
    } else {
        Vec::new()
    };

    // y bounds from points inside the view
    let mut ys: Vec<f64> = smooth_pts
        .iter()
        .chain(raw_pts.iter())
        .flatten()
        .filter(|(x, _)| *x >= x_view.0 && *x <= x_view.1)
        .map(|p| p.1)
        .collect();
    let (full0, full1) = ys.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &y| (a.min(y), b.max(y)));
    let (mut y0, mut y1) = if ys.is_empty() { (0.0, 1.0) } else { (full0, full1) };
    let mut pad_frac = 0.05;
    if o.ignore_outliers {
        // Like TensorBoard: scale to the 5th–95th percentile (+20% padding).
        // Use the full-resolution values in view, not the downsampled
        // min/max points, which deliberately keep the spikes.
        ys = in_view_values(&preps, x_view, with_raw, o.log_y);
        if ys.len() >= 10 {
            let (lo, hi) = (quantile(&mut ys, 0.05), quantile(&mut ys, 0.95));
            if hi > lo {
                (y0, y1) = (lo, hi);
                pad_frac = 0.2;
            }
        }
    }
    if y1 - y0 < 1e-12 {
        let d = if y0.abs() > 1e-12 { y0.abs() * 0.05 } else { 1.0 };
        y0 -= d;
        y1 += d;
    }
    let pad = (y1 - y0) * pad_frac;
    // never pad past the data itself
    (y0, y1) = ((y0 - pad).max(full0 - (full1 - full0) * 0.05), (y1 + pad).min(full1 + (full1 - full0) * 0.05));
    if y1.partial_cmp(&y0) != Some(std::cmp::Ordering::Greater) {
        (y0, y1) = (y0 - 1.0, y0 + 1.0);
    }

    // cursor line
    let cursor_pts: Vec<(f64, f64)> = match o.cursor {
        Some(cx) if cx >= x_view.0 && cx <= x_view.1 => vec![(cx, y0), (cx, y1)],
        _ => Vec::new(),
    };

    let mut datasets = Vec::new();
    if !cursor_pts.is_empty() {
        datasets.push(
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(rgb(0x70, 0x76, 0x80)))
                .data(&cursor_pts),
        );
    }
    for (s, pts) in series.iter().zip(&raw_pts) {
        datasets.push(
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(dim(s.rgb)))
                .data(pts),
        );
    }
    for (s, pts) in series.iter().zip(&smooth_pts) {
        datasets.push(
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(rgb(s.rgb.0, s.rgb.1, s.rgb.2)))
                .data(pts),
        );
    }

    let fx = |v: f64| match o.xmode {
        XMode::Step => fmt_step(v),
        XMode::Relative => fmt_dur(v),
    };
    let fy = |v: f64| if o.log_y { fmt_num(10f64.powf(v)) } else { fmt_num(v) };
    let nx = if chart_area.width > 90 { 5 } else if chart_area.width > 40 { 3 } else { 2 };
    let ny = if chart_area.height > 14 { 5 } else if chart_area.height > 6 { 3 } else { 2 };
    let lab = |a: f64, b: f64, n: usize, f: &dyn Fn(f64) -> String| -> Vec<String> {
        (0..n).map(|i| f(a + (b - a) * i as f64 / (n - 1) as f64)).collect()
    };
    let xl = lab(x_view.0, x_view.1, nx, &fx);
    let yl = lab(y0, y1, ny, &fy);
    // mirror ratatui's Chart layout to know exactly where the plot area is
    let ylw = yl.iter().map(|s| s.chars().count()).max().unwrap_or(0) as u16;
    let x0w = xl.first().map_or(0, |s| s.chars().count()) as u16;
    let left = ylw.max(x0w.saturating_sub(1)).min(chart_area.width / 3);
    let graph = Rect {
        x: chart_area.x + left + 1,
        y: chart_area.y,
        width: chart_area.width.saturating_sub(left + 1),
        height: chart_area.height.saturating_sub(2),
    };
    let axis_style = Style::default().fg(frame());
    let label_style = Style::default().fg(muted());
    let chart = Chart::new(datasets)
        .x_axis(
            Axis::default()
                .bounds([x_view.0, x_view.1])
                .style(axis_style)
                .labels(xl.into_iter().map(|s| Span::styled(s, label_style))),
        )
        .y_axis(
            Axis::default()
                .bounds([y0, y1])
                .style(axis_style)
                .labels(yl.into_iter().map(|s| Span::styled(s, label_style))),
        )
        .legend_position(None);
    chart.render(chart_area, buf);

    // shape markers: at the end of each line and where it crosses the cursor
    if o.markers && graph.width > 2 && graph.height > 1 {
        let to_cell = |x: f64, y: f64| -> Option<(u16, u16)> {
            if !(x_view.0..=x_view.1).contains(&x) || !(y0..=y1).contains(&y) {
                return None;
            }
            let rx = graph.width as f64 * 2.0 - 1.0;
            let ry = graph.height as f64 * 4.0 - 1.0;
            let cx = ((x - x_view.0) * rx / (x_view.1 - x_view.0)).round() as u16 / 2;
            let cy = ((y1 - y) * ry / (y1 - y0)).round() as u16 / 4;
            Some((graph.x + cx.min(graph.width - 1), graph.y + cy.min(graph.height - 1)))
        };
        let ty = |y: f64| if o.log_y { (y > 0.0).then(|| y.log10()) } else { Some(y) };
        for (s, p) in series.iter().zip(&preps) {
            let st = Style::default().fg(rgb(s.rgb.0, s.rgb.1, s.rgb.2)).add_modifier(Modifier::BOLD);
            let mut idxs = Vec::new();
            // last point inside the view
            if let Some(i) = (0..p.xs.len()).rev().find(|&i| p.xs[i] <= x_view.1 && p.xs[i] >= x_view.0) {
                idxs.push(i);
            }
            if let Some(cx) = o.cursor {
                idxs.extend(nearest(&p.xs, cx));
            }
            for i in idxs {
                if let Some((cx, cy)) = ty(p.smooth[i]).and_then(|y| to_cell(p.xs[i], y)) {
                    buf[(cx, cy)].set_char(s.symbol).set_style(st);
                }
            }
        }
    }

    if legend_h > 0 {
        draw_legend(buf, legend_area, series, &preps, o);
    }

    PlotInfo { x_full, x_view, graph }
}

fn draw_legend(buf: &mut Buffer, area: Rect, series: &[Series], preps: &[Prep], o: &PlotOpts) {
    let smoothed = o.smoothing > 0.0;
    let name_w = series.iter().map(|s| s.name.chars().count()).max().unwrap_or(3).clamp(3, 40) as u16;
    // (header, width); columns are dropped from the right when space is short
    let mut cols: Vec<(&str, u16)> = vec![("", 2), ("run", name_w)];
    if smoothed {
        cols.push(("smoothed", 10));
    }
    cols.extend([("value", 10), ("step", 9), ("time", 7), ("min", 10)]);
    let mut used = 0;
    let keep = cols
        .iter()
        .take_while(|(_, w)| {
            used += w + 2;
            used <= area.width + 2
        })
        .count()
        .max(2);
    cols.truncate(keep);
    if used > area.width + 2 && keep == 2 {
        cols[1].1 = area.width.saturating_sub(4);
    }

    let hdr = Style::default().fg(muted()).add_modifier(Modifier::DIM);
    let header = Row::new(cols.iter().map(|c| c.0)).style(hdr);
    let rows = series.iter().zip(preps).map(|(s, p)| {
        let c = rgb(s.rgb.0, s.rgb.1, s.rgb.2);
        let idx = match o.cursor {
            Some(cx) if o.xmode == XMode::Step => nearest(&p.xs, cx),
            Some(cx) => (0..p.xs.len()).min_by(|&a, &b| (p.xs[a] - cx).abs().total_cmp(&(p.xs[b] - cx).abs())),
            None => p.xs.len().checked_sub(1),
        };
        let min = p.raw.iter().copied().filter(|v| v.is_finite()).fold(f64::INFINITY, f64::min);
        let mut cells = vec![
            Span::styled(format!("{}{}", symbols::line::THICK_HORIZONTAL, s.symbol), Style::default().fg(c)),
            Span::styled(s.name.to_string(), Style::default().fg(c)),
        ];
        let strong = Style::default().fg(rgb(0xe6, 0xe6, 0xe6)).add_modifier(Modifier::BOLD);
        if let Some(i) = idx {
            if smoothed {
                cells.push(Span::styled(fmt_num(p.smooth[i]), strong));
            }
            cells.push(if smoothed { Span::raw(fmt_num(p.raw[i])) } else { Span::styled(fmt_num(p.raw[i]), strong) });
            cells.push(Span::styled(s.points[i].step.to_string(), Style::default().fg(muted())));
            cells.push(Span::styled(fmt_dur(s.points[i].wall - s.first_wall), Style::default().fg(muted())));
            cells.push(Span::styled(if min.is_finite() { fmt_num(min) } else { String::new() }, Style::default().fg(muted())));
        }
        cells.truncate(keep);
        Row::new(cells)
    });
    Widget::render(
        Table::new(rows, cols.iter().map(|c| Constraint::Length(c.1))).header(header).column_spacing(2),
        area,
        buf,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_debiased_constant() {
        let s = ema(&[2.0, 2.0, 2.0], 0.9);
        assert!(s.iter().all(|v| (v - 2.0).abs() < 1e-9));
    }

    #[test]
    fn quantiles() {
        let mut v: Vec<f64> = (0..=100).map(f64::from).collect();
        v.reverse();
        assert_eq!(quantile(&mut v, 0.05), 5.0);
        assert_eq!(quantile(&mut v, 0.95), 95.0);
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_num(0.123456), "0.1235");
        assert_eq!(fmt_num(12.5), "12.5");
        assert_eq!(fmt_step(12500.0), "12.5k");
        assert_eq!(fmt_dur(3725.0), "1h02m");
    }
}
