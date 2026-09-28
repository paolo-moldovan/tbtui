//! Discovers runs under local or remote (ssh) log directories and
//! incrementally loads scalar data from their event files.

use crate::events::{parse_event, read_records, split_records};
use crate::remote::{Remote, RemoteSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub step: i64,
    pub wall: f64,
    pub value: f64,
}

#[derive(Default)]
pub struct Run {
    /// local files: bytes consumed so far
    files: BTreeMap<PathBuf, u64>,
    /// remote files: received bytes not yet forming a complete record
    pending: BTreeMap<String, Vec<u8>>,
    pub tags: BTreeMap<String, Vec<Point>>,
    pub first_wall: f64,
}

pub enum Target {
    Local(PathBuf),
    Remote(RemoteSpec),
}

enum Source {
    Local(PathBuf),
    Remote(Remote),
}

pub struct Store {
    sources: Vec<(String, Source)>,
    pub runs: BTreeMap<String, Run>,
    /// last remote error, shown in the UI
    pub error: Option<String>,
}

pub fn is_event_file(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("tfevents"))
}

fn run_name(prefix: &str, rel: &str) -> String {
    match (prefix.is_empty(), rel.is_empty()) {
        (true, true) => ".".to_string(),
        (true, false) => rel.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}/{rel}"),
    }
}

impl Store {
    /// Start all sources. Remote sources connect (possibly prompting for a
    /// password/passphrase on the terminal) and fetch their first batch here.
    pub fn open(targets: Vec<Target>, interval: Duration) -> anyhow::Result<Self> {
        // With several roots, prefix run names with the root's name (like
        // TensorBoard's --logdir_spec).
        let multi = targets.len() > 1;
        let mut store = Store { sources: Vec::new(), runs: BTreeMap::new(), error: None };
        for t in targets {
            match t {
                Target::Local(p) => {
                    let prefix = if multi {
                        p.canonicalize()
                            .ok()
                            .and_then(|c| c.file_name().map(|n| n.to_string_lossy().into_owned()))
                            .unwrap_or_else(|| p.display().to_string())
                    } else {
                        String::new()
                    };
                    store.sources.push((prefix, Source::Local(p)));
                }
                Target::Remote(spec) => {
                    let prefix = if multi { spec.name.clone() } else { String::new() };
                    eprintln!("tbtui: connecting to {} …", spec.display());
                    let (remote, first) = Remote::start(spec, interval)?;
                    store.ingest_remote(&prefix, first);
                    store.sources.push((prefix, Source::Remote(remote)));
                }
            }
        }
        store.refresh();
        Ok(store)
    }

    pub fn has_remote(&self) -> bool {
        self.sources.iter().any(|(_, s)| matches!(s, Source::Remote(_)))
    }

    /// Scan local dirs for new event files, read appended records, and take
    /// in whatever remote batches have arrived. Returns true if data changed.
    pub fn refresh(&mut self) -> bool {
        self.refresh_local() | self.drain_remote()
    }

    /// Ask remote fetchers to poll now instead of waiting for their interval.
    pub fn wake_remote(&self) {
        for (_, s) in &self.sources {
            if let Source::Remote(r) = s {
                r.wake();
            }
        }
    }

    pub fn refresh_local(&mut self) -> bool {
        let mut changed = false;
        for (prefix, src) in &self.sources {
            let Source::Local(root) = src else { continue };
            let files: Vec<PathBuf> = if root.is_file() {
                vec![root.clone()]
            } else {
                WalkDir::new(root)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_file() && is_event_file(e.path()))
                    .map(|e| e.into_path())
                    .collect()
            };
            for f in files {
                let dir = f.parent().unwrap_or(Path::new("."));
                let rel = if root.is_file() {
                    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                } else {
                    dir.strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default()
                };
                let run = self.runs.entry(run_name(prefix, &rel)).or_default();
                changed |= run.load_file(&f);
            }
        }
        changed
    }

    pub fn drain_remote(&mut self) -> bool {
        let mut batches = Vec::new();
        for (prefix, src) in &self.sources {
            if let Source::Remote(r) = src {
                for b in r.poll() {
                    batches.push((prefix.clone(), b));
                }
            }
        }
        let mut changed = false;
        for (prefix, b) in batches {
            match b {
                Ok(chunks) => {
                    self.error = None;
                    changed |= !chunks.is_empty();
                    self.ingest_remote(&prefix, chunks);
                }
                Err(e) => self.error = Some(e),
            }
        }
        changed
    }

    fn ingest_remote(&mut self, prefix: &str, chunks: Vec<crate::remote::Chunk>) {
        for c in chunks {
            // paths arrive relative to the remote root, e.g. "./exp1/events..."
            let rel = c.path.trim_start_matches("./");
            let dir = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            let run = self.runs.entry(run_name(prefix, dir)).or_default();
            run.feed(rel, &c.data, c.reset);
        }
    }

    pub fn all_tags(&self) -> Vec<String> {
        let set: BTreeSet<&String> = self.runs.values().flat_map(|r| r.tags.keys()).collect();
        set.into_iter().cloned().collect()
    }
}

impl Run {
    fn ingest(&mut self, rec: &[u8], touched: &mut BTreeSet<String>) {
        let Some(ev) = parse_event(rec) else { return };
        if ev.wall_time > 0.0 && (self.first_wall == 0.0 || ev.wall_time < self.first_wall) {
            self.first_wall = ev.wall_time;
        }
        for (tag, value) in ev.scalars {
            let series = self.tags.entry(tag.clone()).or_default();
            if series.last().is_some_and(|p| p.step > ev.step) {
                touched.insert(tag);
            }
            series.push(Point { step: ev.step, wall: ev.wall_time, value });
        }
    }

    /// Out-of-order steps (multiple files per run, restarts): keep sorted.
    fn resort(&mut self, touched: BTreeSet<String>) {
        for tag in touched {
            if let Some(s) = self.tags.get_mut(&tag) {
                s.sort_by_key(|p| p.step);
            }
        }
    }

    fn load_file(&mut self, path: &Path) -> bool {
        let mut offset = *self.files.get(path).unwrap_or(&0);
        let before = offset;
        let mut touched = BTreeSet::new();
        let _ = read_records(path, &mut offset, |rec| self.ingest(rec, &mut touched));
        self.files.insert(path.to_path_buf(), offset);
        self.resort(touched);
        offset != before
    }

    fn feed(&mut self, key: &str, data: &[u8], reset: bool) {
        let mut buf = self.pending.remove(key).unwrap_or_default();
        if reset {
            buf.clear();
        }
        buf.extend_from_slice(data);
        let mut touched = BTreeSet::new();
        let used = split_records(&buf, |rec| self.ingest(rec, &mut touched));
        buf.drain(..used);
        self.pending.insert(key.to_string(), buf);
        self.resort(touched);
    }
}
