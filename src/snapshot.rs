//! Non-interactive output: render charts into a buffer and print as ANSI.

use crate::plot::{self, fmt_num, PlotOpts, Series};
use crate::store::Store;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use regex::Regex;
use std::io::{self, Write};

pub struct SnapArgs {
    pub tags: Option<Regex>,
    pub runs: Option<Regex>,
    pub width: u16,
    pub height: u16,
    pub cols: u16,
    pub color: bool,
    pub opts: PlotOpts,
}

pub fn run(store: &Store, a: &SnapArgs) -> anyhow::Result<()> {
    let runs: Vec<(usize, &String, &crate::store::Run)> = store
        .runs
        .iter()
        .enumerate()
        .filter(|(_, (n, _))| a.runs.as_ref().is_none_or(|r| r.is_match(n)))
        .map(|(i, (n, r))| (i, n, r))
        .collect();
    let tags: Vec<String> = store
        .all_tags()
        .into_iter()
        .filter(|t| a.tags.as_ref().is_none_or(|r| r.is_match(t)))
        .collect();
    if tags.is_empty() {
        anyhow::bail!("no scalar tags found (runs: {})", store.runs.len());
    }
    let cols = a.cols.max(1);
    let mut opts = a.opts.clone();
    opts.legend_full = true;
    // grow each chart so the legend can list every run
    let height = a.height + if opts.legend { runs.len() as u16 + 1 } else { 0 };
    let cell_w = a.width / cols;
    let mut out = io::stdout().lock();
    for chunk in tags.chunks(cols as usize) {
        let area = Rect::new(0, 0, cell_w * chunk.len() as u16, height);
        let mut buf = Buffer::empty(area);
        for (i, tag) in chunk.iter().enumerate() {
            let series: Vec<Series> = runs
                .iter()
                .filter_map(|(ci, name, run)| {
                    run.tags.get(tag).map(|pts| Series {
                        name,
                        rgb: plot::run_rgb(*ci),
                        points: pts,
                        first_wall: run.first_wall,
                    })
                })
                .collect();
            let cell = Rect::new(cell_w * i as u16, 0, cell_w, height);
            plot::draw_panel(&mut buf, cell, tag, &series, &opts, false);
        }
        write_buffer(&mut out, &buf, a.color)?;
    }
    Ok(())
}

pub fn write_buffer(out: &mut impl Write, buf: &Buffer, color: bool) -> io::Result<()> {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        let mut cur: Option<(Color, bool)> = None;
        for x in area.left()..area.right() {
            let cell = &buf[(x, y)];
            if color {
                let st = (cell.fg, cell.modifier.contains(Modifier::BOLD));
                if cur != Some(st) {
                    line.push_str("\x1b[0m");
                    if st.1 {
                        line.push_str("\x1b[1m");
                    }
                    line.push_str(&sgr_fg(st.0));
                    cur = Some(st);
                }
            }
            line.push_str(cell.symbol());
        }
        if color {
            line.push_str("\x1b[0m");
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

fn sgr_fg(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m"),
        Color::Indexed(i) => format!("\x1b[38;5;{i}m"),
        Color::Reset => String::new(),
        Color::Black => "\x1b[30m".into(),
        Color::Red => "\x1b[31m".into(),
        Color::Green => "\x1b[32m".into(),
        Color::Yellow => "\x1b[33m".into(),
        Color::Blue => "\x1b[34m".into(),
        Color::Magenta => "\x1b[35m".into(),
        Color::Cyan => "\x1b[36m".into(),
        Color::Gray => "\x1b[37m".into(),
        Color::DarkGray => "\x1b[90m".into(),
        Color::LightRed => "\x1b[91m".into(),
        Color::LightGreen => "\x1b[92m".into(),
        Color::LightYellow => "\x1b[93m".into(),
        Color::LightBlue => "\x1b[94m".into(),
        Color::LightMagenta => "\x1b[95m".into(),
        Color::LightCyan => "\x1b[96m".into(),
        Color::White => "\x1b[97m".into(),
    }
}

/// `tbtui ls`: table of runs × tags with point counts and last values.
pub fn list(store: &Store, tags_re: Option<&Regex>, color: bool) {
    let (b, d, r) = if color { ("\x1b[1m", "\x1b[2m", "\x1b[0m") } else { ("", "", "") };
    for (i, (name, run)) in store.runs.iter().enumerate() {
        let (cr, cg, cb) = plot::run_rgb(i);
        let c = if color { sgr_fg(plot::rgb(cr, cg, cb)) } else { String::new() };
        println!("{c}{b}● {name}{r}  {d}({} tags){r}", run.tags.len());
        let w = run.tags.keys().map(|t| t.len()).max().unwrap_or(0);
        for (tag, pts) in &run.tags {
            if tags_re.is_some_and(|re| !re.is_match(tag)) {
                continue;
            }
            let last = pts.last().unwrap();
            let min = pts.iter().map(|p| p.value).filter(|v| v.is_finite()).fold(f64::INFINITY, f64::min);
            println!(
                "    {tag:<w$}  {d}n={:<7} step={:<9}{r} last={b}{:<10}{r} {d}min={}{r}",
                pts.len(),
                last.step,
                fmt_num(last.value),
                fmt_num(min)
            );
        }
    }
}
