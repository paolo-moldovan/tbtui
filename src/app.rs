//! Interactive, live-updating TUI.

use crate::plot::{self, accent, frame, muted, rgb, PlotInfo, PlotOpts, Series, XMode};
use crate::store::Store;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use regex::{Regex, RegexBuilder};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const SMOOTH_LEVELS: [f64; 8] = [0.0, 0.3, 0.6, 0.8, 0.9, 0.95, 0.98, 0.99];

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Tags,
    Runs,
}

pub struct App {
    store: Store,
    logdir_label: String,
    all_tags: Vec<String>,
    tags: Vec<String>,
    tag_state: ListState,
    runs: Vec<String>,
    run_state: ListState,
    hidden: HashSet<String>,
    colors: HashMap<String, usize>,
    focus: Focus,
    filter: String,
    editing: bool,
    opts: PlotOpts,
    grid: bool,
    help: bool,
    live: bool,
    interval: Duration,
    last_poll: Instant,
    last_change: Instant,
    // layout cache for mouse hit-testing
    tags_area: Rect,
    runs_area: Rect,
    plots: Vec<(usize, Rect, PlotInfo)>,
    quit: bool,
}

impl App {
    pub fn new(store: Store, logdir_label: String, opts: PlotOpts, interval: Duration, filter: String) -> Self {
        let mut app = App {
            store,
            logdir_label,
            all_tags: Vec::new(),
            tags: Vec::new(),
            tag_state: ListState::default().with_selected(Some(0)),
            runs: Vec::new(),
            run_state: ListState::default().with_selected(Some(0)),
            hidden: HashSet::new(),
            colors: HashMap::new(),
            focus: Focus::Tags,
            filter,
            editing: false,
            opts,
            grid: false,
            help: false,
            live: true,
            interval,
            last_poll: Instant::now(),
            last_change: Instant::now(),
            tags_area: Rect::default(),
            runs_area: Rect::default(),
            plots: Vec::new(),
            quit: false,
        };
        app.sync_lists();
        app
    }

    fn filter_re(&self) -> Option<Regex> {
        if self.filter.is_empty() {
            return None;
        }
        RegexBuilder::new(&self.filter)
            .case_insensitive(true)
            .build()
            .or_else(|_| RegexBuilder::new(&regex::escape(&self.filter)).case_insensitive(true).build())
            .ok()
    }

    /// Rebuild tag/run lists after data or filter changes, keeping selection by name.
    fn sync_lists(&mut self) {
        let cur_tag = self.selected_tag().map(str::to_owned);
        self.all_tags = self.store.all_tags();
        let re = self.filter_re();
        self.tags = self.all_tags.iter().filter(|t| re.as_ref().is_none_or(|r| r.is_match(t))).cloned().collect();
        let idx = cur_tag.and_then(|t| self.tags.iter().position(|x| *x == t)).unwrap_or(0);
        self.tag_state.select(if self.tags.is_empty() { None } else { Some(idx.min(self.tags.len() - 1)) });

        self.runs = self.store.runs.keys().cloned().collect();
        for r in &self.runs {
            let n = self.colors.len();
            self.colors.entry(r.clone()).or_insert(n);
        }
        if self.run_state.selected().is_none_or(|i| i >= self.runs.len()) {
            self.run_state.select(if self.runs.is_empty() { None } else { Some(0) });
        }
    }

    fn selected_tag(&self) -> Option<&str> {
        self.tag_state.selected().and_then(|i| self.tags.get(i)).map(String::as_str)
    }

    fn series_for(&self, tag: &str) -> Vec<Series<'_>> {
        self.store
            .runs
            .iter()
            .filter(|(n, _)| !self.hidden.contains(*n))
            .filter_map(|(n, run)| {
                run.tags.get(tag).map(|pts| Series {
                    name: n,
                    rgb: plot::run_rgb(self.colors.get(n).copied().unwrap_or(0)),
                    points: pts,
                    first_wall: run.first_wall,
                })
            })
            .collect()
    }

    fn poll_data(&mut self, force: bool) {
        let mut changed = false;
        if force {
            self.store.wake_remote();
        }
        if force || (self.live && self.last_poll.elapsed() >= self.interval) {
            self.last_poll = Instant::now();
            changed |= self.store.refresh_local();
        }
        // remote batches arrive from background threads whenever ready
        if self.live || force {
            changed |= self.store.drain_remote();
        }
        if changed {
            self.last_change = Instant::now();
            self.sync_lists();
        }
    }

    // ------------------------------------------------------------ main loop

    pub fn run(mut self, term: &mut DefaultTerminal) -> anyhow::Result<()> {
        execute!(std::io::stdout(), EnableMouseCapture)?;
        let res = (|| -> anyhow::Result<()> {
            let mut dirty = true;
            while !self.quit {
                if dirty {
                    term.draw(|f| self.draw(f))?;
                    dirty = false;
                }
                let mut wait = if self.live {
                    self.interval.saturating_sub(self.last_poll.elapsed()).min(Duration::from_millis(1000))
                } else {
                    Duration::from_millis(1000)
                };
                if self.live && self.store.has_remote() {
                    wait = wait.min(Duration::from_millis(250));
                }
                if event::poll(wait)? {
                    // drain queued events (e.g. fast mouse motion) before redrawing
                    loop {
                        match event::read()? {
                            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
                            Event::Mouse(m) => self.on_mouse(m),
                            _ => {}
                        }
                        if self.quit || !event::poll(Duration::ZERO)? {
                            break;
                        }
                    }
                    dirty = true;
                }
                let before = self.last_change;
                self.poll_data(false);
                // redraw for new data, and once a second for the "updated Xs ago" clock
                dirty |= self.last_change != before || self.live;
            }
            Ok(())
        })();
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        res
    }

    // ------------------------------------------------------------ input

    fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.editing {
            match k.code {
                KeyCode::Enter => self.editing = false,
                KeyCode::Esc => {
                    self.editing = false;
                    self.filter.clear();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.sync_lists();
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Tags { Focus::Runs } else { Focus::Tags }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::Home => self.move_sel(i32::MIN / 2),
            KeyCode::End => self.move_sel(i32::MAX / 2),
            KeyCode::Char(' ') | KeyCode::Enter if self.focus == Focus::Runs => self.toggle_run(),
            KeyCode::Char('a') => {
                if self.hidden.is_empty() {
                    self.hidden = self.runs.iter().cloned().collect();
                } else {
                    self.hidden.clear();
                }
            }
            KeyCode::Char('i') => {
                if let Some(r) = self.run_state.selected().and_then(|i| self.runs.get(i)) {
                    let r = r.clone();
                    self.hidden = self.runs.iter().filter(|x| **x != r).cloned().collect();
                }
            }
            KeyCode::Char('/') => {
                self.editing = true;
                self.focus = Focus::Tags;
            }
            KeyCode::Char('s') => self.smooth_step(1),
            KeyCode::Char('S') => self.smooth_step(-1),
            KeyCode::Char('y') => self.opts.log_y = !self.opts.log_y,
            KeyCode::Char('x') => {
                self.opts.xmode = match self.opts.xmode {
                    XMode::Step => XMode::Relative,
                    XMode::Relative => XMode::Step,
                };
                self.opts.x_range = None;
                self.opts.cursor = None;
            }
            KeyCode::Char('o') => self.opts.ignore_outliers = !self.opts.ignore_outliers,
            KeyCode::Char('u') => self.opts.show_raw = !self.opts.show_raw,
            KeyCode::Char('g') => self.grid = !self.grid,
            KeyCode::Left | KeyCode::Char('h') => self.move_cursor(if shift { -10.0 } else { -1.0 }),
            KeyCode::Right | KeyCode::Char('l') => self.move_cursor(if shift { 10.0 } else { 1.0 }),
            KeyCode::Char('H') => self.move_cursor(-10.0),
            KeyCode::Char('L') => self.move_cursor(10.0),
            KeyCode::Esc | KeyCode::Char('c') => self.opts.cursor = None,
            KeyCode::Char('+') | KeyCode::Char('=') => self.zoom(0.7, None),
            KeyCode::Char('-') | KeyCode::Char('_') => self.zoom(1.0 / 0.7, None),
            KeyCode::Char('[') => self.pan(-0.2),
            KeyCode::Char(']') => self.pan(0.2),
            KeyCode::Char('0') => self.opts.x_range = None,
            KeyCode::Char('r') => self.poll_data(true),
            KeyCode::Char('p') => self.live = !self.live,
            _ => {}
        }
    }

    fn move_sel(&mut self, d: i32) {
        let (state, len) = match self.focus {
            Focus::Tags => (&mut self.tag_state, self.tags.len()),
            Focus::Runs => (&mut self.run_state, self.runs.len()),
        };
        if len == 0 {
            return;
        }
        let cur = state.selected().unwrap_or(0) as i64;
        state.select(Some((cur + d as i64).clamp(0, len as i64 - 1) as usize));
    }

    fn toggle_run(&mut self) {
        if let Some(r) = self.run_state.selected().and_then(|i| self.runs.get(i))
            && !self.hidden.remove(r) {
                self.hidden.insert(r.clone());
            }
    }

    fn smooth_step(&mut self, d: i32) {
        let cur = SMOOTH_LEVELS.iter().position(|&l| l >= self.opts.smoothing - 1e-9).unwrap_or(0) as i32;
        self.opts.smoothing = SMOOTH_LEVELS[(cur + d).clamp(0, SMOOTH_LEVELS.len() as i32 - 1) as usize];
    }

    /// Plot info of the focused chart (selected tag).
    fn focused_info(&self) -> Option<PlotInfo> {
        let sel = self.tag_state.selected()?;
        self.plots.iter().find(|(i, _, _)| *i == sel).or(self.plots.first()).map(|p| p.2)
    }

    fn move_cursor(&mut self, cells: f64) {
        let Some(info) = self.focused_info() else { return };
        let (a, b) = info.x_view;
        let dx = (b - a) / info.graph.width.max(1) as f64;
        let c = match self.opts.cursor {
            Some(c) => c + cells * dx,
            None => {
                if cells < 0.0 {
                    b
                } else {
                    a
                }
            }
        };
        self.opts.cursor = Some(c.clamp(a, b));
    }

    fn zoom(&mut self, factor: f64, center: Option<f64>) {
        let Some(info) = self.focused_info() else { return };
        let (a, b) = info.x_view;
        let c = center.or(self.opts.cursor.filter(|c| *c >= a && *c <= b)).unwrap_or((a + b) / 2.0);
        let (na, nb) = (c - (c - a) * factor, c + (b - c) * factor);
        let (fa, fb) = info.x_full;
        if nb - na >= fb - fa {
            self.opts.x_range = None;
        } else if nb - na > 1e-9 {
            // keep the window inside the data where possible
            let shift = if na < fa { fa - na } else if nb > fb { fb - nb } else { 0.0 };
            self.opts.x_range = Some((na + shift, nb + shift));
        }
    }

    fn pan(&mut self, frac: f64) {
        let Some(info) = self.focused_info() else { return };
        let Some((a, b)) = self.opts.x_range else { return };
        let (fa, fb) = info.x_full;
        let d = ((b - a) * frac).clamp(fa - a, fb - b);
        self.opts.x_range = Some((a + d, b + d));
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        let hit_plot = self.plots.iter().find(|(_, r, _)| r.contains(pos)).copied();
        let x_at = |info: &PlotInfo| {
            let g = info.graph;
            let f = (m.column.saturating_sub(g.x) as f64 / g.width.max(1) as f64).clamp(0.0, 1.0);
            info.x_view.0 + f * (info.x_view.1 - info.x_view.0)
        };
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                if let Some((idx, _, info)) = hit_plot {
                    self.tag_state.select(Some(idx));
                    self.zoom(if up { 0.8 } else { 1.25 }, Some(x_at(&info)));
                } else if self.tags_area.contains(pos) || self.runs_area.contains(pos) {
                    self.focus = if self.tags_area.contains(pos) { Focus::Tags } else { Focus::Runs };
                    self.move_sel(if up { -1 } else { 1 });
                }
            }
            MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((_, _, info)) = hit_plot
                    && info.graph.contains(pos) {
                        self.opts.cursor = Some(x_at(&info));
                    }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((idx, _, info)) = hit_plot {
                    self.tag_state.select(Some(idx));
                    if info.graph.contains(pos) {
                        self.opts.cursor = Some(x_at(&info));
                    }
                } else if self.tags_area.contains(pos) {
                    self.focus = Focus::Tags;
                    let row = (m.row.saturating_sub(self.tags_area.y + 1)) as usize + self.tag_state.offset();
                    if row < self.tags.len() {
                        self.tag_state.select(Some(row));
                    }
                } else if self.runs_area.contains(pos) {
                    self.focus = Focus::Runs;
                    let row = (m.row.saturating_sub(self.runs_area.y + 1)) as usize + self.run_state.offset();
                    if row < self.runs.len() {
                        self.run_state.select(Some(row));
                        self.toggle_run();
                    }
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self, f: &mut Frame) {
        let [top, body, bottom] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(5), Constraint::Length(1)]).areas(f.area());
        let side_w = (self.all_tags.iter().chain(&self.runs).map(|s| s.chars().count()).max().unwrap_or(10) as u16 + 6)
            .clamp(22, 40)
            .min(body.width / 3);
        let [side, main] = Layout::horizontal([Constraint::Length(side_w), Constraint::Min(20)]).areas(body);
        let [tags_a, runs_a] = Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(side);
        self.tags_area = tags_a;
        self.runs_area = runs_a;

        self.draw_top(f, top);
        self.draw_tags(f, tags_a);
        self.draw_runs(f, runs_a);
        self.draw_main(f, main);
        self.draw_bottom(f, bottom);
        if self.help {
            draw_help(f);
        }
    }

    fn draw_top(&self, f: &mut Frame, area: Rect) {
        let n_pts: usize = self.store.runs.values().flat_map(|r| r.tags.values()).map(Vec::len).sum();
        let live = if self.live {
            Span::styled(" ● LIVE ", Style::default().fg(rgb(0x10, 0x10, 0x10)).bg(rgb(0x5c, 0xd6, 0x7a)).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(" ❚❚ PAUSED ", Style::default().fg(rgb(0x10, 0x10, 0x10)).bg(rgb(0xff, 0xd1, 0x4f)).add_modifier(Modifier::BOLD))
        };
        let ago = self.last_change.elapsed().as_secs();
        let stats = Span::styled(
            format!(
                "  {} runs · {} tags · {} points · updated {} ago  ",
                self.runs.len(),
                self.all_tags.len(),
                n_pts,
                plot::fmt_dur(ago as f64)
            ),
            Style::default().fg(muted()),
        );
        let stats = match &self.store.error {
            Some(e) => Span::styled(format!("  ⚠ {e}  "), Style::default().fg(rgb(0xff, 0x5c, 0x7a)).add_modifier(Modifier::BOLD)),
            None => stats,
        };
        let right = Line::from(vec![stats, live]);
        // shorten the path from the left so the stats always fit
        let room = (area.width as usize).saturating_sub(right.width() + 9);
        let n = self.logdir_label.chars().count();
        let label = if n > room {
            let tail: String = self.logdir_label.chars().skip(n - room.saturating_sub(1)).collect();
            format!("…{tail}")
        } else {
            self.logdir_label.clone()
        };
        let line = Line::from(vec![
            Span::styled(" tbtui ", Style::default().fg(rgb(0x10, 0x10, 0x10)).bg(accent()).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(label, Style::default().fg(rgb(0xe6, 0xe6, 0xe6)).add_modifier(Modifier::BOLD)),
        ]);
        f.render_widget(Paragraph::new(right.right_aligned()), area);
        f.render_widget(Paragraph::new(line), area);
    }

    fn side_block(&self, title: String, focused: bool) -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if focused { accent() } else { frame() }))
            .title(Span::styled(
                title,
                Style::default().fg(if focused { accent() } else { muted() }).add_modifier(Modifier::BOLD),
            ))
    }

    fn draw_tags(&mut self, f: &mut Frame, area: Rect) {
        let mut title = format!(" Tags {}/{} ", self.tags.len(), self.all_tags.len());
        if !self.filter.is_empty() || self.editing {
            title.push_str(&format!("/{}{} ", self.filter, if self.editing { "▏" } else { "" }));
        }
        let items: Vec<ListItem> = self.tags.iter().map(|t| ListItem::new(t.as_str())).collect();
        let focused = self.focus == Focus::Tags;
        let list = List::new(items)
            .block(self.side_block(title, focused))
            .style(Style::default().fg(rgb(0xc8, 0xcc, 0xd2)))
            .highlight_style(if focused {
                Style::default().fg(rgb(0x10, 0x10, 0x10)).bg(accent()).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(accent()).add_modifier(Modifier::BOLD)
            })
            .highlight_symbol("▸ ");
        f.render_stateful_widget(list, area, &mut self.tag_state);
    }

    fn draw_runs(&mut self, f: &mut Frame, area: Rect) {
        let vis = self.runs.len() - self.runs.iter().filter(|r| self.hidden.contains(*r)).count();
        let title = format!(" Runs {}/{} ", vis, self.runs.len());
        let items: Vec<ListItem> = self
            .runs
            .iter()
            .map(|r| {
                let (cr, cg, cb) = plot::run_rgb(self.colors.get(r).copied().unwrap_or(0));
                let hidden = self.hidden.contains(r);
                let (mark, st) = if hidden {
                    ("□ ", Style::default().fg(muted()).add_modifier(Modifier::DIM))
                } else {
                    ("■ ", Style::default().fg(rgb(cr, cg, cb)))
                };
                ListItem::new(Line::from(vec![
                    Span::styled(mark, st),
                    Span::styled(r.as_str(), if hidden { st } else { Style::default().fg(rgb(0xd8, 0xdc, 0xe2)) }),
                ]))
            })
            .collect();
        let focused = self.focus == Focus::Runs;
        let list = List::new(items)
            .block(self.side_block(title, focused))
            .highlight_style(if focused {
                Style::default().bg(rgb(0x3a, 0x3f, 0x48)).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
            .highlight_symbol(if focused { "▸ " } else { "  " });
        f.render_stateful_widget(list, area, &mut self.run_state);
    }

    fn draw_main(&mut self, f: &mut Frame, area: Rect) {
        self.plots.clear();
        if self.tags.is_empty() {
            let msg = if self.store.runs.is_empty() {
                "No event files found yet (looking for *tfevents*). Waiting for data…"
            } else {
                "No tags match the filter."
            };
            let p = Paragraph::new(Line::from(Span::styled(msg, Style::default().fg(muted()))).centered()).block(
                Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(frame())),
            );
            f.render_widget(p, area);
            return;
        }
        let sel = self.tag_state.selected().unwrap_or(0);
        if !self.grid {
            let tag = self.tags[sel].clone();
            let series = self.series_for(&tag);
            let info = plot::draw_panel(f.buffer_mut(), area, &tag, &series, &self.opts, true);
            self.plots.push((sel, area, info));
            return;
        }
        let n = self.tags.len() as u16;
        let cols = (area.width / 56).clamp(1, n.max(1));
        let rows = (area.height / 14).clamp(1, n.div_ceil(cols).max(1));
        let per_page = (cols * rows) as usize;
        let page = sel / per_page;
        let pages = self.tags.len().div_ceil(per_page);
        let mut opts = self.opts.clone();
        opts.legend = false;
        let row_areas = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows as usize]).split(area);
        for (r, row_area) in row_areas.iter().enumerate() {
            let cells = Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols as usize]).split(*row_area);
            for (c, cell) in cells.iter().enumerate() {
                let idx = page * per_page + r * cols as usize + c;
                let Some(tag) = self.tags.get(idx).cloned() else { continue };
                let series = self.series_for(&tag);
                let title = if pages > 1 && idx == page * per_page {
                    format!("{tag}  [page {}/{}]", page + 1, pages)
                } else {
                    tag
                };
                let info = plot::draw_panel(f.buffer_mut(), *cell, &title, &series, &opts, idx == sel);
                self.plots.push((idx, *cell, info));
            }
        }
    }

    fn draw_bottom(&self, f: &mut Frame, area: Rect) {
        let key = |k: &str| Span::styled(format!(" {k} "), Style::default().fg(rgb(0x10, 0x10, 0x10)).bg(rgb(0x9a, 0xa0, 0xaa)));
        let desc = |d: &str| Span::styled(format!(" {d}  "), Style::default().fg(muted()));
        let line = if self.editing {
            Line::from(vec![key("filter"), Span::raw(format!(" /{}▏ ", self.filter)), desc("regex · Enter keep · Esc clear")])
        } else {
            Line::from(vec![
                key("↑↓"),
                desc("tag"),
                key("tab"),
                desc("runs"),
                key("←→"),
                desc("cursor"),
                key("+-[]0"),
                desc("zoom/pan"),
                key("s/S"),
                desc(&format!("smooth {:.2}", self.opts.smoothing)),
                key("y"),
                desc("log"),
                key("g"),
                desc("grid"),
                key("/"),
                desc("filter"),
                key("?"),
                desc("help"),
                key("q"),
                desc("quit"),
            ])
        };
        f.render_widget(Paragraph::new(line), area);
    }
}

fn draw_help(f: &mut Frame) {
    let rows: &[(&str, &str)] = &[
        ("↑ ↓  j k  PgUp PgDn", "select tag / run"),
        ("Tab", "switch focus: tags ↔ runs"),
        ("Space / Enter / click", "toggle run visibility"),
        ("a  /  i", "show-all ↔ hide-all  /  isolate selected run"),
        ("/", "filter tags (regex, case-insensitive)"),
        ("g", "grid view: all filtered tags at once"),
        ("← → h l  (Shift: ×10)", "move value cursor · mouse hover works too"),
        ("Esc / c", "clear cursor"),
        ("+ -  scroll wheel", "zoom x-axis (around cursor/mouse)"),
        ("[ ]  /  0", "pan  /  reset zoom"),
        ("s / S", "more / less smoothing (EMA, TensorBoard-style)"),
        ("u", "show/hide faint unsmoothed lines"),
        ("y", "toggle log y-axis"),
        ("o", "ignore outliers in y-range"),
        ("x", "x-axis: step ↔ relative time"),
        ("r  /  p", "reload now  /  pause live updates"),
        ("q / Ctrl-C", "quit"),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("  {k:<24}"), Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
                Span::styled(d.to_string(), Style::default().fg(rgb(0xd8, 0xdc, 0xe2))),
            ])
        })
        .collect();
    let a = f.area();
    let w = 76.min(a.width);
    let h = (rows.len() as u16 + 4).min(a.height);
    let r = Rect::new(a.x + (a.width - w) / 2, a.y + (a.height - h) / 2, w, h);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(accent()))
                .title(Span::styled(" keys ", Style::default().fg(accent()).add_modifier(Modifier::BOLD)))
                .title_bottom(Line::from(Span::styled(" any key to close ", Style::default().fg(muted()))).right_aligned())
                .padding(ratatui::widgets::Padding::vertical(1)),
        ),
        r,
    );
}
