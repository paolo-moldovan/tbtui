//! Interactive, live-updating TUI.

use crate::filter::Filter;
use crate::plot::{self, accent, frame, muted, rgb, PlotInfo, PlotOpts, Series, XMode};
use crate::store::Store;
use crate::tree::{self, Row};
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
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

const SMOOTH_LEVELS: [f64; 8] = [0.0, 0.3, 0.6, 0.8, 0.9, 0.95, 0.98, 0.99];
/// smallest chart cell in the grid (columns, rows)
const MIN_CELL: (u16, u16) = (34, 9);

#[derive(PartialEq, Clone, Copy, Debug)]
enum Pane {
    Tags,
    Runs,
    Charts,
}

#[derive(PartialEq, Clone, Copy)]
enum Drag {
    Sidebar,
    Split,
}

#[derive(PartialEq, Clone, Copy)]
enum InputMode {
    Regex,
    Example,
}

/// The open filter editor for the tag or run tree.
struct Editor {
    pane: Pane,
    input: String,
    mode: InputMode,
    /// filter before editing, restored on Esc
    saved: Filter,
}

#[derive(Default)]
struct TreePane {
    items: Vec<String>,
    filter: Filter,
    collapsed: HashSet<String>,
    rows: Vec<Row>,
    state: ListState,
    area: Rect,
}

impl TreePane {
    /// While editing its filter a tree shows every item (non-matches dimmed)
    /// so more examples can be picked; otherwise non-matches are hidden.
    fn rebuild(&mut self, editing: bool) {
        let cur = self.cur().map(|r| r.path.clone());
        let keep: Vec<bool> = self.items.iter().map(|i| editing || self.filter.matches(i)).collect();
        self.rows = tree::build(&self.items, &keep, &self.collapsed);
        let idx = cur.and_then(|p| self.rows.iter().position(|r| r.path == p)).unwrap_or(0);
        self.state.select(if self.rows.is_empty() { None } else { Some(idx.min(self.rows.len() - 1)) });
    }

    fn cur(&self) -> Option<&Row> {
        self.state.selected().and_then(|i| self.rows.get(i))
    }

    fn move_by(&mut self, d: i64) {
        if self.rows.is_empty() {
            return;
        }
        let c = self.state.selected().unwrap_or(0) as i64;
        self.state.select(Some((c + d).clamp(0, self.rows.len() as i64 - 1) as usize));
    }

    fn names(&self, row: &Row) -> Vec<String> {
        row.leaves.iter().map(|&i| self.items[i].clone()).collect()
    }

    fn cur_names(&self) -> Vec<String> {
        self.cur().map(|r| self.names(r)).unwrap_or_default()
    }

    fn matched(&self) -> usize {
        self.items.iter().filter(|i| self.filter.matches(i)).count()
    }

    fn set_collapsed(&mut self, path: &str, collapsed: bool, editing: bool) {
        if collapsed {
            self.collapsed.insert(path.to_string());
        } else {
            self.collapsed.remove(path);
        }
        self.rebuild(editing);
    }

    /// ← : collapse the group, or jump to the parent group
    fn left(&mut self, editing: bool) {
        let Some(sel) = self.state.selected() else { return };
        let r = self.rows[sel].clone();
        if r.is_group() && r.expanded {
            self.set_collapsed(&r.path, true, editing);
        } else if let Some(p) = tree::parent(&self.rows, sel) {
            self.state.select(Some(p));
        }
    }

    /// → : expand the group, or step into it
    fn right(&mut self, editing: bool) {
        let Some(r) = self.cur().cloned() else { return };
        if r.is_group() && !r.expanded {
            self.set_collapsed(&r.path, false, editing);
        } else if r.is_group() {
            self.move_by(1);
        }
    }

    fn toggle_collapse(&mut self, editing: bool) {
        if let Some(r) = self.cur().cloned().filter(Row::is_group) {
            self.set_collapsed(&r.path, r.expanded, editing);
        }
    }

    fn row_at(&self, y: u16) -> Option<usize> {
        if y <= self.area.y || y + 1 >= self.area.bottom() {
            return None;
        }
        let i = (y - self.area.y - 1) as usize + self.state.offset();
        (i < self.rows.len()).then_some(i)
    }

    fn set_all_collapsed(&mut self, collapse: bool, editing: bool) {
        self.collapsed.clear();
        if collapse {
            // with everything expanded, every group row is visible: collapse them all
            self.rebuild(editing);
            let groups: Vec<String> = self.rows.iter().filter(|r| r.is_group()).map(|r| r.path.clone()).collect();
            self.collapsed.extend(groups);
        }
        self.rebuild(editing);
    }
}

pub struct App {
    store: Store,
    logdir_label: String,
    tags: TreePane,
    runs: TreePane,
    pinned: BTreeSet<String>,
    hidden: HashSet<String>,
    colors: HashMap<String, usize>,
    focus: Pane,
    editor: Option<Editor>,
    opts: PlotOpts,
    show_all: bool,
    help: bool,
    live: bool,
    interval: Duration,
    last_poll: Instant,
    last_change: Instant,
    // layout (user-adjustable)
    sidebar: bool,
    side_w: Option<u16>,
    split_pct: u16,
    grid_cols: u16,
    focus_panel: usize,
    drag: Option<Drag>,
    // layout cache for mouse hit-testing
    side_area: Rect,
    plots: Vec<(usize, Rect, PlotInfo)>,
    quit: bool,
}

impl App {
    pub fn new(store: Store, logdir_label: String, opts: PlotOpts, interval: Duration, tag_filter: &str, run_filter: &str) -> Self {
        let mut app = App {
            store,
            logdir_label,
            tags: TreePane { filter: Filter::new(tag_filter), ..Default::default() },
            runs: TreePane { filter: Filter::new(run_filter), ..Default::default() },
            pinned: BTreeSet::new(),
            hidden: HashSet::new(),
            colors: HashMap::new(),
            focus: Pane::Tags,
            editor: None,
            opts,
            show_all: false,
            help: false,
            live: true,
            interval,
            last_poll: Instant::now(),
            last_change: Instant::now(),
            sidebar: true,
            side_w: None,
            split_pct: 60,
            grid_cols: 0,
            focus_panel: 0,
            drag: None,
            side_area: Rect::default(),
            plots: Vec::new(),
            quit: false,
        };
        app.sync_lists();
        app
    }

    fn editing(&self, p: Pane) -> bool {
        self.editor.as_ref().is_some_and(|e| e.pane == p)
    }

    fn tree(&mut self, p: Pane) -> &mut TreePane {
        if p == Pane::Runs { &mut self.runs } else { &mut self.tags }
    }

    fn rebuild(&mut self, p: Pane) {
        let editing = self.editing(p);
        self.tree(p).rebuild(editing);
    }

    /// Refresh tag/run lists after new data, keeping cursor positions.
    fn sync_lists(&mut self) {
        self.tags.items = self.store.all_tags();
        self.runs.items = self.store.runs.keys().cloned().collect();
        for r in &self.runs.items {
            let n = self.colors.len();
            self.colors.entry(r.clone()).or_insert(n);
        }
        self.rebuild(Pane::Tags);
        self.rebuild(Pane::Runs);
    }

    fn run_visible(&self, name: &str) -> bool {
        !self.hidden.contains(name) && self.runs.filter.matches(name)
    }

    /// Tags shown in the chart area: everything (g), else the pinned tags,
    /// else the highlighted tag, or every tag of the highlighted group.
    fn displayed(&self) -> Vec<String> {
        if self.show_all {
            return self.tags.items.iter().filter(|t| self.tags.filter.matches(t)).cloned().collect();
        }
        if !self.pinned.is_empty() {
            return self.pinned.iter().filter(|t| self.tags.items.contains(t)).cloned().collect();
        }
        self.tags.cur_names()
    }

    fn series_for(&self, tag: &str) -> Vec<Series<'_>> {
        self.store
            .runs
            .iter()
            .filter(|(n, _)| self.run_visible(n))
            .filter_map(|(n, run)| {
                let ci = self.colors.get(n).copied().unwrap_or(0);
                run.tags.get(tag).map(|pts| Series {
                    name: n,
                    rgb: plot::run_rgb(ci),
                    symbol: plot::run_symbol(ci),
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
                // redraw for new data, and regularly for the "updated Xs ago" clock
                dirty |= self.last_change != before || self.live;
            }
            Ok(())
        })();
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        res
    }

    // ------------------------------------------------------------ keys

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.editor.is_some() {
            self.on_editor_key(k);
            return;
        }
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        // global keys
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab => self.cycle_focus(1),
            KeyCode::BackTab => self.cycle_focus(-1),
            KeyCode::Char('/') => self.open_editor(None),
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
            KeyCode::Char('m') => self.opts.markers = !self.opts.markers,
            KeyCode::Char('P') => plot::cycle_palette(),
            KeyCode::Char('g') => {
                self.show_all = !self.show_all;
                self.focus_panel = 0;
            }
            KeyCode::Char('b') => {
                self.sidebar = !self.sidebar;
                if !self.sidebar {
                    self.focus = Pane::Charts;
                }
            }
            KeyCode::Char('<') => self.resize_sidebar(-2),
            KeyCode::Char('>') => self.resize_sidebar(2),
            KeyCode::Char('{') => self.split_pct = self.split_pct.saturating_sub(5).max(15),
            KeyCode::Char('}') => self.split_pct = (self.split_pct + 5).min(85),
            KeyCode::Char(',') => self.grid_cols = self.current_cols().saturating_sub(1).max(1),
            KeyCode::Char('.') => self.grid_cols = (self.current_cols() + 1).min(12),
            KeyCode::Char(';') => self.grid_cols = 0,
            KeyCode::Char('H') => self.move_cursor(-10.0),
            KeyCode::Char('L') => self.move_cursor(10.0),
            KeyCode::Char('c') => self.opts.cursor = None,
            KeyCode::Char('+') | KeyCode::Char('=') => self.zoom(0.7, None),
            KeyCode::Char('-') | KeyCode::Char('_') => self.zoom(1.0 / 0.7, None),
            KeyCode::Char('[') => self.pan(-0.2),
            KeyCode::Char(']') => self.pan(0.2),
            KeyCode::Char('0') => self.opts.x_range = None,
            KeyCode::Char('r') => self.poll_data(true),
            KeyCode::Char('p') => self.live = !self.live,
            KeyCode::Char('C') => {
                let p = self.tree_pane_of_focus();
                let editing = self.editing(p);
                let collapse = self.tree(p).collapsed.is_empty();
                self.tree(p).set_all_collapsed(collapse, editing);
            }
            KeyCode::Esc => {
                if self.opts.cursor.is_some() {
                    self.opts.cursor = None;
                } else if self.focus != Pane::Charts && self.tree_of_focus().filter.is_active() {
                    let p = self.focus;
                    self.tree(p).filter.clear();
                    self.rebuild(p);
                } else if !self.pinned.is_empty() {
                    self.pinned.clear();
                }
            }
            _ => match self.focus {
                Pane::Tags | Pane::Runs => self.on_tree_key(k),
                Pane::Charts => self.on_chart_key(k),
            },
        }
    }

    fn tree_pane_of_focus(&self) -> Pane {
        if self.focus == Pane::Runs { Pane::Runs } else { Pane::Tags }
    }

    fn tree_of_focus(&self) -> &TreePane {
        if self.focus == Pane::Runs { &self.runs } else { &self.tags }
    }

    fn cycle_focus(&mut self, d: i32) {
        let order: &[Pane] = if self.sidebar { &[Pane::Tags, Pane::Runs, Pane::Charts] } else { &[Pane::Charts] };
        let i = order.iter().position(|p| *p == self.focus).unwrap_or(0) as i32;
        self.focus = order[(i + d).rem_euclid(order.len() as i32) as usize];
    }

    fn on_tree_key(&mut self, k: KeyEvent) {
        let p = self.focus;
        let editing = self.editing(p);
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.tree(p).move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.tree(p).move_by(1),
            KeyCode::PageUp => self.tree(p).move_by(-10),
            KeyCode::PageDown => self.tree(p).move_by(10),
            KeyCode::Home => self.tree(p).move_by(i64::MIN / 2),
            KeyCode::End => self.tree(p).move_by(i64::MAX / 2),
            KeyCode::Left | KeyCode::Char('h') => self.tree(p).left(editing),
            KeyCode::Right | KeyCode::Char('l') => self.tree(p).right(editing),
            KeyCode::Enter => {
                if self.tree(p).cur().is_some_and(Row::is_group) {
                    self.tree(p).toggle_collapse(editing);
                } else {
                    self.toggle_mark(p);
                }
            }
            KeyCode::Char(' ') => self.toggle_mark(p),
            KeyCode::Char('e') => {
                let names = self.tree(p).cur_names();
                self.open_editor(Some(names));
            }
            KeyCode::Char('a') if p == Pane::Runs => {
                if self.hidden.is_empty() {
                    self.hidden = self.runs.items.iter().cloned().collect();
                } else {
                    self.hidden.clear();
                }
            }
            KeyCode::Char('a') => {
                if self.pinned.is_empty() {
                    self.pinned = self.tags.items.iter().filter(|t| self.tags.filter.matches(t)).cloned().collect();
                } else {
                    self.pinned.clear();
                }
            }
            KeyCode::Char('i') if p == Pane::Runs => {
                let keep: HashSet<String> = self.runs.cur_names().into_iter().collect();
                self.hidden = self.runs.items.iter().filter(|r| !keep.contains(*r)).cloned().collect();
            }
            _ => {}
        }
    }

    /// Space: pin/unpin tags, show/hide runs (whole group at once).
    fn toggle_mark(&mut self, p: Pane) {
        let names = self.tree(p).cur_names();
        if p == Pane::Runs {
            if names.iter().any(|n| !self.hidden.contains(n)) {
                self.hidden.extend(names);
            } else {
                for n in &names {
                    self.hidden.remove(n);
                }
            }
        } else if names.iter().all(|n| self.pinned.contains(n)) {
            for n in &names {
                self.pinned.remove(n);
            }
        } else {
            self.pinned.extend(names);
        }
        self.focus_panel = self.focus_panel.min(self.displayed().len().saturating_sub(1));
    }

    fn on_chart_key(&mut self, k: KeyEvent) {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let n = self.displayed().len();
        let per_page = self.plots.len().max(1);
        match k.code {
            KeyCode::Left | KeyCode::Char('h') => self.move_cursor(if shift { -10.0 } else { -1.0 }),
            KeyCode::Right | KeyCode::Char('l') => self.move_cursor(if shift { 10.0 } else { 1.0 }),
            KeyCode::Up | KeyCode::Char('k') => self.focus_panel = self.focus_panel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.focus_panel = (self.focus_panel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.focus_panel = self.focus_panel.saturating_sub(per_page),
            KeyCode::PageDown => self.focus_panel = (self.focus_panel + per_page).min(n.saturating_sub(1)),
            KeyCode::Char(' ') => {
                // unpin the focused chart
                if let Some(t) = self.displayed().get(self.focus_panel) {
                    self.pinned.remove(t);
                }
            }
            _ => {}
        }
    }

    fn resize_sidebar(&mut self, d: i16) {
        let cur = self.side_area.width.max(12) as i16;
        self.side_w = Some((cur + d).max(12) as u16);
        self.sidebar = true;
    }

    fn current_cols(&self) -> u16 {
        let first_row_y = self.plots.first().map(|p| p.1.y);
        self.plots.iter().filter(|p| Some(p.1.y) == first_row_y).count() as u16
    }

    fn smooth_step(&mut self, d: i32) {
        let cur = SMOOTH_LEVELS.iter().position(|&l| l >= self.opts.smoothing - 1e-9).unwrap_or(0) as i32;
        self.opts.smoothing = SMOOTH_LEVELS[(cur + d).clamp(0, SMOOTH_LEVELS.len() as i32 - 1) as usize];
    }

    // ------------------------------------------------------------ filter editor

    fn open_editor(&mut self, examples: Option<Vec<String>>) {
        let pane = self.tree_pane_of_focus();
        self.focus = pane;
        self.sidebar = true;
        let saved = self.tree(pane).filter.clone();
        if let Some(names) = examples {
            self.tree(pane).filter.toggle_examples(&names);
        }
        let input = self.tree(pane).filter.text.clone();
        self.editor = Some(Editor { pane, input, mode: InputMode::Regex, saved });
        self.rebuild(pane);
    }

    fn close_editor(&mut self, keep: bool) {
        if let Some(e) = self.editor.take() {
            if !keep {
                self.tree(e.pane).filter = e.saved;
            }
            self.rebuild(e.pane);
        }
    }

    /// Keep the regex input box in sync after examples / grex options change.
    fn sync_editor_input(&mut self) {
        let Some(pane) = self.editor.as_ref().map(|e| e.pane) else { return };
        let t = self.tree(pane).filter.text.clone();
        if let Some(e) = self.editor.as_mut().filter(|e| e.mode == InputMode::Regex) {
            e.input = t;
        }
    }

    fn on_editor_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some((pane, mode)) = self.editor.as_ref().map(|e| (e.pane, e.mode)) else { return };
        match k.code {
            KeyCode::Esc => return self.close_editor(false),
            KeyCode::Char('c') if ctrl => return self.close_editor(false),
            KeyCode::Enter => {
                let e = self.editor.as_mut().unwrap();
                if e.mode == InputMode::Example && !e.input.is_empty() {
                    let ex = std::mem::take(&mut e.input);
                    self.tree(pane).filter.add_example(&ex);
                } else {
                    return self.close_editor(true);
                }
            }
            KeyCode::Tab => {
                let names = self.tree(pane).cur_names();
                self.tree(pane).filter.toggle_examples(&names);
                self.sync_editor_input();
            }
            KeyCode::Up => self.tree(pane).move_by(-1),
            KeyCode::Down => self.tree(pane).move_by(1),
            KeyCode::PageUp => self.tree(pane).move_by(-10),
            KeyCode::PageDown => self.tree(pane).move_by(10),
            KeyCode::Left => self.tree(pane).left(true),
            KeyCode::Right => self.tree(pane).right(true),
            KeyCode::Char('t') if ctrl => {
                let e = self.editor.as_mut().unwrap();
                e.mode = if mode == InputMode::Regex { InputMode::Example } else { InputMode::Regex };
                e.input.clear();
                self.sync_editor_input();
            }
            KeyCode::Char(c @ ('d' | 'w' | 'r' | 'a' | 'u')) if ctrl => {
                let f = &mut self.tree(pane).filter;
                match c {
                    'd' => f.grex.digits = !f.grex.digits,
                    'w' => f.grex.words = !f.grex.words,
                    'r' => f.grex.repetitions = !f.grex.repetitions,
                    'a' => f.grex.anchors = !f.grex.anchors,
                    _ => f.clear(),
                }
                if !f.examples.is_empty() {
                    f.regenerate();
                }
                self.sync_editor_input();
            }
            KeyCode::Backspace | KeyCode::Char(_) if !ctrl => {
                let e = self.editor.as_mut().unwrap();
                match k.code {
                    KeyCode::Char(c) => e.input.push(c),
                    _ => {
                        e.input.pop();
                    }
                }
                if mode == InputMode::Regex {
                    // typing a regex by hand detaches it from the examples
                    let t = e.input.clone();
                    let f = &mut self.tree(pane).filter;
                    f.examples.clear();
                    f.set_text(&t);
                }
            }
            _ => {}
        }
        self.rebuild(pane);
    }

    // ------------------------------------------------------------ chart navigation

    fn focused_info(&self) -> Option<PlotInfo> {
        self.plots.iter().find(|p| p.0 == self.focus_panel).or(self.plots.first()).map(|p| p.2)
    }

    fn move_cursor(&mut self, cells: f64) {
        let Some(info) = self.focused_info() else { return };
        let (a, b) = info.x_view;
        let dx = (b - a) / info.graph.width.max(1) as f64;
        let c = match self.opts.cursor {
            Some(c) => c + cells * dx,
            None if cells < 0.0 => b,
            None => a,
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

    // ------------------------------------------------------------ mouse

    fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        let side = self.side_area;
        let (tags_a, runs_a) = (self.tags.area, self.runs.area);
        let in_rows = m.row >= side.y && m.row < side.bottom();
        let on_vbar = self.sidebar && in_rows && (m.column + 1 == side.right() || m.column == side.right());
        let on_hbar = self.sidebar && side.contains(pos) && (m.row + 1 == tags_a.bottom() || m.row == runs_a.y) && !on_vbar;
        let hit_plot = self.plots.iter().find(|(_, r, _)| r.contains(pos)).copied();
        let x_at = |info: &PlotInfo| {
            let g = info.graph;
            let f = (m.column.saturating_sub(g.x) as f64 / g.width.saturating_sub(1).max(1) as f64).clamp(0.0, 1.0);
            info.x_view.0 + f * (info.x_view.1 - info.x_view.0)
        };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) if on_vbar => self.drag = Some(Drag::Sidebar),
            MouseEventKind::Down(MouseButton::Left) if on_hbar => self.drag = Some(Drag::Split),
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_some() => match self.drag {
                Some(Drag::Sidebar) => self.side_w = Some(m.column.saturating_sub(side.x) + 1),
                Some(Drag::Split) if side.height > 0 => {
                    let pct = (m.row.saturating_sub(side.y) + 1) as u32 * 100 / side.height as u32;
                    self.split_pct = pct.clamp(15, 85) as u16;
                }
                _ => {}
            },
            MouseEventKind::Up(_) => self.drag = None,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                if let Some((idx, _, info)) = hit_plot {
                    self.focus_panel = idx;
                    self.zoom(if up { 0.8 } else { 1.25 }, Some(x_at(&info)));
                } else if tags_a.contains(pos) || runs_a.contains(pos) {
                    let p = if tags_a.contains(pos) { Pane::Tags } else { Pane::Runs };
                    self.tree(p).move_by(if up { -1 } else { 1 });
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
                    self.focus_panel = idx;
                    if self.editor.is_none() {
                        self.focus = Pane::Charts;
                    }
                    if info.graph.contains(pos) {
                        self.opts.cursor = Some(x_at(&info));
                    }
                } else if tags_a.contains(pos) || runs_a.contains(pos) {
                    let p = if tags_a.contains(pos) { Pane::Tags } else { Pane::Runs };
                    self.click_tree(p, m.column, m.row);
                }
            }
            _ => {}
        }
    }

    /// Click in a tree: the arrow folds, the mark pins/shows-hides, anywhere
    /// selects. While that tree's filter editor is open, a click toggles an example.
    fn click_tree(&mut self, p: Pane, x: u16, y: u16) {
        let Some(i) = self.tree(p).row_at(y) else { return };
        if self.editor.as_ref().is_some_and(|e| e.pane != p) {
            return;
        }
        if self.editor.is_none() {
            self.focus = p;
        }
        self.tree(p).state.select(Some(i));
        if self.editing(p) {
            let names = self.tree(p).cur_names();
            self.tree(p).filter.toggle_examples(&names);
            self.sync_editor_input();
            self.rebuild(p);
            return;
        }
        let tp = self.tree(p);
        let row = &tp.rows[i];
        // content starts after the border (1) and the highlight symbol (2)
        let col = x.saturating_sub(tp.area.x + 3) as usize;
        let indent = row.depth * 2;
        if row.is_group() && (indent..indent + 2).contains(&col) {
            tp.toggle_collapse(false);
        } else if (indent + 2..indent + 4).contains(&col) {
            self.toggle_mark(p);
        }
    }

    // ------------------------------------------------------------ drawing

    fn draw(&mut self, f: &mut Frame) {
        let editor_h = if self.editor.is_some() { 5 } else { 0 };
        let [top, body, editor_area, bottom] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(editor_h),
            Constraint::Length(1),
        ])
        .areas(f.area());
        let auto_w = self
            .tags
            .rows
            .iter()
            .chain(&self.runs.rows)
            .map(|r| r.depth * 2 + r.label.chars().count() + 12)
            .max()
            .unwrap_or(20)
            .clamp(22, 44) as u16;
        let max_w = body.width.saturating_sub(30).max(12);
        let side_w = if self.sidebar { self.side_w.unwrap_or(auto_w.min(body.width / 3)).clamp(12, max_w) } else { 0 };
        let [side, main] = Layout::horizontal([Constraint::Length(side_w), Constraint::Min(20)]).areas(body);
        self.side_area = side;
        if self.sidebar {
            let [tags_a, runs_a] =
                Layout::vertical([Constraint::Percentage(self.split_pct), Constraint::Percentage(100 - self.split_pct)])
                    .areas(side);
            self.draw_tree(f, tags_a, Pane::Tags);
            self.draw_tree(f, runs_a, Pane::Runs);
        } else {
            self.tags.area = Rect::default();
            self.runs.area = Rect::default();
        }
        self.draw_top(f, top);
        self.draw_main(f, main);
        if self.editor.is_some() {
            self.draw_editor(f, editor_area);
        }
        self.draw_bottom(f, bottom);
        if self.help {
            draw_help(f);
        }
    }

    fn draw_top(&self, f: &mut Frame, area: Rect) {
        let n_pts: usize = self.store.runs.values().flat_map(|r| r.tags.values()).map(Vec::len).sum();
        let dark = rgb(0x10, 0x10, 0x10);
        let live = if self.live {
            Span::styled(" ● LIVE ", Style::default().fg(dark).bg(rgb(0x5c, 0xd6, 0x7a)).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(" ❚❚ PAUSED ", Style::default().fg(dark).bg(rgb(0xff, 0xd1, 0x4f)).add_modifier(Modifier::BOLD))
        };
        let ago = self.last_change.elapsed().as_secs();
        let stats = match &self.store.error {
            Some(e) => Span::styled(format!("  ⚠ {e}  "), Style::default().fg(rgb(0xff, 0x5c, 0x7a)).add_modifier(Modifier::BOLD)),
            None => Span::styled(
                format!(
                    "  {} runs · {} tags · {} points · updated {} ago  ",
                    self.runs.items.len(),
                    self.tags.items.len(),
                    n_pts,
                    plot::fmt_dur(ago as f64)
                ),
                Style::default().fg(muted()),
            ),
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
            Span::styled(" tbtui ", Style::default().fg(dark).bg(accent()).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(label, Style::default().fg(rgb(0xe6, 0xe6, 0xe6)).add_modifier(Modifier::BOLD)),
        ]);
        f.render_widget(Paragraph::new(right.right_aligned()), area);
        f.render_widget(Paragraph::new(line), area);
    }

    fn draw_tree(&mut self, f: &mut Frame, area: Rect, p: Pane) {
        let editing = self.editing(p);
        let focused = self.focus == p;
        let text = rgb(0xd8, 0xdc, 0xe2);
        let dimmed = Style::default().fg(rgb(0x5a, 0x60, 0x6a));
        let tp = if p == Pane::Runs { &self.runs } else { &self.tags };
        let name = if p == Pane::Runs { "Runs" } else { "Tags" };
        let mut title = format!(" {name} {}/{} ", tp.matched(), tp.items.len());
        if tp.filter.is_active() {
            let t: String = tp.filter.text.chars().take(area.width.saturating_sub(16) as usize).collect();
            title.push_str(&format!("/{t} "));
        }
        let items: Vec<ListItem> = tp
            .rows
            .iter()
            .map(|r| {
                let names: Vec<&String> = r.leaves.iter().map(|&i| &tp.items[i]).collect();
                let matches = !editing || names.iter().any(|n| tp.filter.matches(n));
                let arrow = match (r.is_group(), r.expanded) {
                    (true, true) => "▾ ",
                    (true, false) => "▸ ",
                    _ => "  ",
                };
                let mark = if p == Pane::Runs {
                    let vis = names.iter().filter(|n| !self.hidden.contains(**n)).count();
                    match r.leaf {
                        Some(i) => {
                            let ci = self.colors.get(&tp.items[i]).copied().unwrap_or(0);
                            let (cr, cg, cb) = plot::run_rgb(ci);
                            let st = if vis > 0 { Style::default().fg(rgb(cr, cg, cb)) } else { dimmed };
                            Span::styled(format!("{} ", plot::run_symbol(ci)), st)
                        }
                        None => Span::styled(
                            if vis == names.len() { "✔ " } else if vis > 0 { "◐ " } else { "· " },
                            Style::default().fg(muted()),
                        ),
                    }
                } else {
                    let pinned = names.iter().filter(|n| self.pinned.contains(**n)).count();
                    if pinned == 0 {
                        Span::styled("○ ", dimmed)
                    } else if pinned == names.len() {
                        Span::styled("● ", Style::default().fg(accent()))
                    } else {
                        Span::styled("◐ ", Style::default().fg(accent()))
                    }
                };
                let label_style = match (matches, r.is_group()) {
                    (false, _) => dimmed,
                    (true, true) => Style::default().fg(rgb(0xa8, 0xb0, 0xbc)).add_modifier(Modifier::BOLD),
                    (true, false) => Style::default().fg(text),
                };
                let mut spans = vec![
                    Span::raw("  ".repeat(r.depth)),
                    Span::styled(arrow, Style::default().fg(muted())),
                    mark,
                    Span::styled(r.label.clone(), label_style),
                ];
                if r.is_group() {
                    spans.push(Span::styled(format!(" {}", names.len()), dimmed));
                }
                if editing && names.iter().all(|n| tp.filter.is_example(n)) {
                    spans.push(Span::styled(" ◆", Style::default().fg(accent()).add_modifier(Modifier::BOLD)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if focused { accent() } else { frame() }))
            .title(Span::styled(title, Style::default().fg(if focused { accent() } else { muted() }).add_modifier(Modifier::BOLD)));
        let list = List::new(items)
            .block(block)
            .highlight_style(if focused {
                Style::default().bg(rgb(0x3a, 0x3f, 0x48)).add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(rgb(0x26, 0x29, 0x30))
            })
            .highlight_symbol(if focused { "▸ " } else { "  " })
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
        let tp = self.tree(p);
        tp.area = area;
        f.render_stateful_widget(list, area, &mut tp.state);
    }

    fn draw_main(&mut self, f: &mut Frame, area: Rect) {
        self.plots.clear();
        let disp = self.displayed();
        if disp.is_empty() {
            let msg = if self.store.runs.is_empty() {
                "No event files found yet (looking for *tfevents*). Waiting for data…"
            } else {
                "Nothing selected: highlight a tag, or pin some with Space."
            };
            let p = Paragraph::new(Line::from(Span::styled(msg, Style::default().fg(muted()))).centered()).block(
                Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(frame())),
            );
            f.render_widget(p, area);
            return;
        }
        self.focus_panel = self.focus_panel.min(disp.len() - 1);
        let (cols, rows) = grid_dims(disp.len(), area.width, area.height, self.grid_cols);
        let per_page = (cols * rows) as usize;
        let page = self.focus_panel / per_page;
        let pages = disp.len().div_ceil(per_page);
        let shown = &disp[page * per_page..disp.len().min((page + 1) * per_page)];
        let rows = (shown.len() as u16).div_ceil(cols).max(1);
        let mut opts = self.opts.clone();
        opts.legend = area.height / rows >= 16;
        let row_areas = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows as usize]).split(area);
        for (i, tag) in shown.iter().enumerate() {
            let (r, c) = (i / cols as usize, i % cols as usize);
            let cells = Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols as usize]).split(row_areas[r]);
            let idx = page * per_page + i;
            let series = self.series_for(tag);
            let title = if pages > 1 && i == 0 { format!("{tag}  [page {}/{}]", page + 1, pages) } else { tag.clone() };
            let hl = idx == self.focus_panel && (self.focus == Pane::Charts || disp.len() == 1);
            let info = plot::draw_panel(f.buffer_mut(), cells[c], &title, &series, &opts, hl);
            self.plots.push((idx, cells[c], info));
        }
    }

    fn draw_editor(&self, f: &mut Frame, area: Rect) {
        let Some(e) = &self.editor else { return };
        let tp = if e.pane == Pane::Runs { &self.runs } else { &self.tags };
        let fl = &tp.filter;
        let bright = Style::default().fg(rgb(0xe6, 0xe6, 0xe6));
        let key = |k: &str| Span::styled(k.to_string(), Style::default().fg(accent()).add_modifier(Modifier::BOLD));
        let dimt = |t: &str| Span::styled(t.to_string(), Style::default().fg(muted()));
        let on = |b: bool| if b { Span::styled("✓", Style::default().fg(rgb(0x5c, 0xd6, 0x7a))) } else { dimt("·") };
        let cursor = Span::styled("▏", Style::default().fg(accent()));
        let mut regex_line = vec![dimt("regex    ")];
        if e.mode == InputMode::Regex {
            regex_line.push(Span::styled(e.input.clone(), bright.add_modifier(Modifier::BOLD)));
            regex_line.push(cursor.clone());
        } else {
            regex_line.push(Span::styled(fl.text.clone(), bright));
        }
        if fl.invalid {
            regex_line.push(Span::styled("  (invalid regex: matching literally)", Style::default().fg(rgb(0xff, 0x5c, 0x7a))));
        }
        let mut ex_line = vec![dimt("examples")];
        for x in &fl.examples {
            ex_line.push(Span::styled(format!("  ◆ {x}"), Style::default().fg(accent())));
        }
        if e.mode == InputMode::Example {
            ex_line.push(dimt("  + "));
            ex_line.push(Span::styled(e.input.clone(), bright.add_modifier(Modifier::BOLD)));
            ex_line.push(cursor);
        } else if fl.examples.is_empty() {
            ex_line.push(dimt("  none: Tab/click adds the highlighted item, ^T types one; grex builds the regex"));
        }
        let mut opts_line = vec![dimt("grex     ")];
        for (k, label, v) in [
            ("^D", " digits→\\d ", fl.grex.digits),
            ("^W", " words→\\w ", fl.grex.words),
            ("^R", " repeats ", fl.grex.repetitions),
            ("^A", " whole name ", fl.grex.anchors),
        ] {
            opts_line.extend([key(k), dimt(label), on(v), dimt("   ")]);
        }
        opts_line.extend([
            key("^U"),
            dimt(" clear   "),
            key("^T"),
            dimt(if e.mode == InputMode::Regex { " type an example   " } else { " type a regex   " }),
            key("Enter"),
            dimt(if e.mode == InputMode::Example { " add / done   " } else { " done   " }),
            key("Esc"),
            dimt(" cancel"),
        ]);
        let name = if e.pane == Pane::Runs { "runs" } else { "tags" };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(accent()))
            .title(Span::styled(format!(" filter {name} "), Style::default().fg(accent()).add_modifier(Modifier::BOLD)))
            .title(Line::from(dimt(&format!(" {}/{} match ", tp.matched(), tp.items.len()))).right_aligned());
        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(vec![Line::from(regex_line), Line::from(ex_line), Line::from(opts_line)]).block(block), area);
    }

    fn draw_bottom(&self, f: &mut Frame, area: Rect) {
        let key = |k: &str| Span::styled(format!(" {k} "), Style::default().fg(rgb(0x10, 0x10, 0x10)).bg(rgb(0x9a, 0xa0, 0xaa)));
        let desc = |d: &str| Span::styled(format!(" {d}  "), Style::default().fg(muted()));
        let mut spans = Vec::new();
        let mut add = |k: &str, d: &str| {
            spans.push(key(k));
            spans.push(desc(d));
        };
        if self.editor.is_some() {
            add("↑↓", "move");
            add("Tab", "add/remove example");
            add("Enter", "done");
            add("Esc", "cancel");
        } else {
            match self.focus {
                Pane::Tags => {
                    add("␣", "pin");
                    add("←→", "fold");
                    add("a", "pin all/none");
                }
                Pane::Runs => {
                    add("␣", "show/hide");
                    add("i", "isolate");
                    add("a", "all/none");
                }
                Pane::Charts => {
                    add("←→", "cursor");
                    add("↑↓", "chart");
                    add("␣", "unpin");
                }
            }
            add("/ e", "filter");
            add("tab", "pane");
            add("+-", "zoom");
            add("s", &format!("smooth {:.2}", self.opts.smoothing));
            add("g", if self.show_all { "all ✓" } else { "all" });
            add("?", "help");
        }
        let mode = if self.show_all {
            "all tags".to_string()
        } else if !self.pinned.is_empty() {
            format!("{} pinned", self.pinned.len())
        } else {
            "selection".to_string()
        };
        let cols = if self.grid_cols > 0 { format!(" · {} cols", self.grid_cols) } else { String::new() };
        let right = Line::from(Span::styled(format!(" {mode}{cols} · {} ", plot::palette_name()), Style::default().fg(muted())));
        f.render_widget(Paragraph::new(right.right_aligned()), area);
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

/// Columns and rows for `n` charts: fit as many as possible (then paginate),
/// preferring cells around 3:1 (width:height in character cells).
fn grid_dims(n: usize, w: u16, h: u16, cols_override: u16) -> (u16, u16) {
    let n = n.max(1) as u16;
    let max_rows = (h / MIN_CELL.1).max(1);
    if cols_override > 0 {
        let cols = cols_override.min(n);
        return (cols, n.div_ceil(cols).min(max_rows));
    }
    let max_cols = (w / MIN_CELL.0).max(1);
    let mut best = (1, 1);
    let mut best_score = (0u16, f64::MIN);
    for cols in 1..=max_cols.min(n) {
        let rows = n.div_ceil(cols).min(max_rows);
        let shown = (cols * rows).min(n);
        let score = (shown, ((w / cols) as f64 / 3.0).min((h / rows) as f64));
        if score.0 > best_score.0 || (score.0 == best_score.0 && score.1 > best_score.1) {
            best_score = score;
            best = (cols, rows);
        }
    }
    best
}

fn draw_help(f: &mut Frame) {
    let rows: &[(&str, &str)] = &[
        ("Tab / Shift-Tab", "focus: tags → runs → charts"),
        ("↑↓ jk  ←→ hl  Enter", "move · fold/unfold tree groups"),
        ("Space / click mark", "tags: pin (group = all) · runs: show/hide"),
        ("a  /  i", "pin all↔none, runs all↔none  /  isolate run(s)"),
        ("C", "collapse / expand the whole tree"),
        ("/", "filter the focused tree (regex, or grex examples)"),
        ("e", "filter by example, starting from the highlighted item"),
        ("Esc", "clear cursor → clear filter → unpin all"),
        ("g", "show all (filtered) tags as a grid"),
        ("charts: ←→ ↑↓ PgUp/Dn", "cursor · focused chart · page"),
        ("H L  / mouse hover", "move cursor ×10 · cursor follows mouse"),
        ("+ -  wheel  [ ]  0", "zoom x · pan · reset"),
        ("< >  { }", "sidebar width · tags/runs split (or drag borders)"),
        (", .  ;", "fewer / more grid columns · automatic"),
        ("b", "hide/show sidebar"),
        ("s / S", "more / less smoothing (EMA, TensorBoard-style)"),
        ("o", "ignore outliers: y-axis fits 5th–95th percentile"),
        ("y  x  u  m", "log-y · step↔time · raw lines · markers"),
        ("P", "palette (okabe-ito, tol-bright: colorblind-safe)"),
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
    let w = 80.min(a.width);
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

#[cfg(test)]
mod tests {
    use super::grid_dims;

    #[test]
    fn grid_fits() {
        assert_eq!(grid_dims(1, 120, 40, 0), (1, 1));
        let (c, r) = grid_dims(4, 200, 50, 0);
        assert!(c * r >= 4);
        // too many for the screen: paginate with the most that fit
        let (c, r) = grid_dims(100, 120, 40, 0);
        assert_eq!((c, r), (3, 4));
        assert_eq!(grid_dims(6, 200, 50, 2), (2, 3));
    }
}
